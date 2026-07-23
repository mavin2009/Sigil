# Why Sigil

*Or: what you would have to hold in your head to write this by hand.*

Sigil does not make Rust unnecessary. It generates Rust — safe, idiomatic,
lock-free Rust. The argument for the language is not that the generated code
is exotic. **It is that the generated code is obvious in hindsight and
extremely easy to get subtly wrong by hand**, and that the properties it
satisfies are checked rather than reviewed.

This document walks one real component — the secrets vault from
`examples/security/vault.sigil` — from source, to generated Rust, to the
list of things a reviewer would otherwise have to verify by reading.

---

## The whole component

**51 lines** of Sigil (excluding comments) describe a four-stage,
authenticated, authorized, audited secret-release path:

```
process Audit {
  state recorded: Int = 0

  on request: Request {
    let logged = request ~> write_audit @timeout(60.ms) @retry(2) @recover(with: deny_unaudited)
    recorded := recorded + 1
    send logged to Vault
  }
}
```

That compiles to **479 lines** of Rust (`342` in the library, `137` in the
runnable driver). The ratio is not the point. The point is *which* 439 lines,
and what happens if any one of them is wrong.

---

## What one handler becomes

The three-line handler above generates this:

```rust
pub async fn on_request(&mut self, request: Request) -> Result<()> {
    let logged = {
        let __in = request.clone();
        let mut __attempt: u64 = 0;
        loop {
            match timeout(Duration::from_millis(60), write_audit(__in.clone())).await {
                Ok(Ok(v)) => break v,
                _ if __attempt < 2 => {
                    __attempt += 1;
                    sigil_rt::chaos::note_retry("write_audit");
                }
                _ => {
                    sigil_rt::chaos::note_recovery("write_audit");
                    break deny_unaudited(__in).await?;
                }
            }
        }
    };
    self.recorded = (self.recorded + 1);
    match self.vault_out.as_mut() {
        Some(out) => {
            sigil_rt::backpressure::block(out.round_robin().raw(), logged).await?;
        }
        None => return Err(sigil_rt::SigilError::Transform("outbox to Vault not connected".into())),
    }
    Ok(())
}
```

Every line here is a decision a human would have to make correctly, in every
handler, forever:

| Generated detail | The bug if you get it wrong |
| ---------------- | --------------------------- |
| `timeout(...)` wraps the call | A hung audit sink hangs the request path indefinitely |
| the retry loop is **bounded** by a counter | `loop { retry }` on a persistently failing dependency is an outage amplifier |
| the fallback runs **after** retries are exhausted, not instead of them | Silent loss of retryable requests |
| the fallback is `deny_unaudited`, a **pure** transform | A fallback that can itself fail or hang reintroduces the failure it exists to absorb |
| `recorded += 1` sits **above** the `send` | Audit-after-forward: a secret can be served with no audit record |
| the send is matched, not `.unwrap()`ed | Panic in an actor task, silently killing a shard |
| the send's queue-full behaviour is **declared** | An unbounded wait that quietly invalidates your latency SLO, or an unbounded queue that quietly becomes an outage |

Sigil checks all seven. The first four are Level-1 rules, the fifth is the
Level-4 ORDERING obligation, the sixth is simply not expressible, and the
seventh is the declared back-pressure policy that `require path_latency`
holds you to.

---

## What the process becomes

```rust
pub struct Authn {
    pub verified: i64,
    /// Outbox to the Authz actor. Wire with connect_authz() before spawn.
    pub authz_out: Option<sigil_rt::Router<AuthzHandle>>,
}

pub struct AuthnHandle {
    tx: tokio::sync::mpsc::Sender<Request>,
}

impl Authn {
    pub fn spawn(mut self, capacity: usize)
        -> (AuthnHandle, tokio::task::JoinHandle<(Self, sigil_rt::ActorStats)>)
    {
        let (tx, mut rx) = tokio::sync::mpsc::channel::<Request>(capacity);
        let join = tokio::spawn(async move {
            let mut stats = sigil_rt::ActorStats::default();
            while let Some(msg) = rx.recv().await {
                match self.on_request(msg).await {
                    Ok(()) => stats.handled += 1,
                    Err(_) => stats.dropped += 1,
                }
            }
            self.authz_out = None; // release downstream channel
            (self, stats)
        });
        (AuthnHandle { tx }, join)
    }
}
```

Note what is **absent**: no `Arc`, no `Mutex`, no `AtomicU64`, no `unsafe`.
`spawn` takes `self` **by move**, so after the call the state is
unreachable except by message. Data races are not prevented by discipline
here; they are unrepresentable. An integration test asserts none of those
tokens ever appear in generated code.

Now consider the accumulator in `examples/finance/clearing.sigil`:

```
state settled_value: Float = 0.0
```

Rust has no atomic float. The hand-written equivalent of a shared settlement
total is `Arc<Mutex<f64>>` — which means lock ordering, contention on the hot
path, and the ever-present risk of holding the guard across an `.await`.
Sigil's answer is that the value never needs sharing: it lives inside one
actor's task and comes back at `join()`.

---

## What the topology becomes

Four `send` statements generate the entire wiring, spawn order, and shutdown
sequence:

```rust
// sinks first, so upstream stages can be wired to live handles
for _i in 0..shards { let (h, j) = Vault::new().spawn(1024); ... }
for _i in 0..shards {
    let mut inst = Audit::new();
    inst.connect_vault(vault_handles.clone());
    let (h, j) = inst.spawn(1024);
    ...
}
// ... Authz, Authn

drop(authn_handles);   // stage 1 drains
// ... its actors release their outboxes, cascading downstream
drop(authz_handles);
drop(audit_handles);
drop(vault_handles);
```

Shutdown ordering is where hand-written actor systems deadlock. Drop the
handles in the wrong order and you either hang forever waiting on a stage
that will never drain, or you strand in-flight messages in a closed channel.
Sigil derives the order topologically and proves the graph is acyclic first;
a cyclic `send` graph is a compile error, not a 3 a.m. page.

---

## The properties, and where they come from

For the vault, the compiler establishes:

```
hold Vault.served   <= Audit.recorded     PROVEN (Level 4)
hold Audit.recorded <= Authz.granted      PROVEN (Level 4)
hold Authz.granted  <= Authn.verified     PROVEN (Level 4)
require path_timeout_sum <= 700.ms        500ms actual (Level 2)
```

Composed: **a secret cannot be served unless an authenticated, authorized
request was audited first.** That is the sentence a security reviewer
actually cares about, and it is a build artifact rather than a claim.

The proof is structural — base ordering, per-message deltas, update-before-
send, all-paths-through, static send multiplicity — and needs **no fairness
or liveness assumption**. Every failure mode the language admits (timeouts,
`@error` drops, guard rejections, staged shutdown) only *decreases* the
downstream count, so the inequality survives all of them.

Try to break it and the compiler stops you:

```
$ # move `recorded := recorded + 1` below the send
$ cargo run -p sigilc -- examples/security/vault.sigil out --level 4

error[Level 4 (system)]: Level-4 violation in spec 'ZeroTrust': ORDERING
fails — the `request` handler of `Audit` sends toward `Vault` BEFORE
updating `recorded`; a message could arrive uncounted. Move the update
above the send.
```

```
$ # make the deny path an external transform (it could now fail or hang)
error[Level 4 (system)]: Level-3 requires infallible recovery:
`deny_unauthorized` used as a @recover target but declared external (empty
body). A fallback that can fail or hang reintroduces the loss it exists to
prevent — give it a pure body.
```

Both mistakes are the kind that pass code review. Neither passes the build.

---

## Where to go next

- [Language Reference](LANGUAGE.md) — the complete surface
- [Assurance Levels](ASSURANCE.md) — what each level proves, and the 25
  programs that must fail to compile
- [Runtime & Generated Code](RUNTIME.md) — the actor model in detail

## The honest part

Sigil does **not** claim the component cannot fail. It claims:

1. Specific failure classes are unrepresentable (data races, shared mutable
   accumulators, null, untagged failure paths, cyclic actor graphs).
2. Specific properties are proven (the invariants above, the latency budget).
3. **Everything else is named.** Every build emits `RESIDUAL_RISK.md`
   listing what was assumed rather than proven: the external transforms
   (the real KMS, policy engine, audit sink), the OS, the scheduler,
   channel-capacity tuning.

That last point is the design's whole ethos. A `--level 0` build states
plainly that it established nothing. A `--level 4` build lists exactly which
inequalities were proven and which assumptions were *enforced at runtime* by
generated guards. Nothing is quietly assumed.

---

## Measured, not asserted

Both flagship components run under fault injection — 20% of external calls
fail, latency spikes past every timeout:

```
$ SIGIL_CHAOS_FAIL_PCT=20 SIGIL_CHAOS_LATENCY_MS=120 cargo run --bin demo

[Authn]  verified = 1920   handled + dropped = 1920 + 0
[Authz]  granted  = 1920   handled + dropped = 1920 + 0
[Audit]  recorded = 1920   handled + dropped = 1920 + 0
[Vault]  served   = 1920   handled + dropped = 1920 + 0
chaos: 10240 external calls, 1757 injected faults, 2560 retries,
       632 recover paths taken
```

1,757 faults hit the system. 2,560 retries and 632 recoveries absorbed them.
Zero messages lost, every invariant intact, no locks anywhere.

The finance component behaves the same way:

```
[Ingest]     admitted = 1920      [Clearing]   netted  = 1920
[RiskGate]   passed   = 1920      [Settlement] settled = 1920
             exposure = 1920.0                 settled_value = 1920.0
```

`settled <= admitted` is not observed to hold in this run. It was proven
before the run started, and the run is a witness.
