"""Generate programs whose invariants the Level 3/4 provers will try to prove.

Shapes are chosen to stress exactly where the provers reason: conditional
counters, guarded and unguarded sends, mutated vs immutable guard operands,
fan-out, clamping, and multi-handler dispatch. Any program the compiler
PROVES is then executed; the generated demo asserts the proven invariants,
so a violation is a prover unsoundness rather than a number to eyeball.
"""
import random, sys

def gen(seed):
    r = random.Random(seed)
    L = []
    L.append("schema M { id: String, n: Int, v: Float }")
    L.append("transform ext(m: M) -> M {}")
    L.append("transform pure_f(m: M) -> M { m }")

    nproc = r.randint(2, 3)
    procs = [f"P{i}" for i in range(nproc)]
    holds = []

    for i, p in enumerate(procs):
        body = [f"process {p} {{", f"  state c{i}: Int = 0", f"  state k{i}: Int = 0", f"  state a{i}: Int = 0"]
        body.append("  on m: M {")
        if r.random() < 0.6:
            body.append("    let z = m ~> ext @timeout(30.ms) @recover(with: pure_f)")
            src = "z"
        else:
            src = "m"

        # counter: unconditional or conditional
        cond_style = r.choice(["uncond", "cond_msg", "cond_state"])
        if cond_style == "uncond":
            body.append(f"    c{i} := c{i} + 1")
            cguard = None
        elif cond_style == "cond_msg":
            body.append(f"    c{i} := c{i} + if {src}.n > 0 {{ 1 }} else {{ 0 }}")
            cguard = f"{src}.n > 0"
        else:
            # guard operand is STATE the handler also mutates — the shape that
            # broke the prover once.
            body.append(f"    c{i} := c{i} + if k{i} > 0 {{ 1 }} else {{ 0 }}")
            cguard = f"k{i} > 0"

        body.append(f"    k{i} := k{i} + 1")

        # Exact integer accumulation with a two-sided lower clamp.
        if r.random() < 0.5:
            body.append(f"    a{i} := a{i} + if {src}.n < 0 {{ 0 }} else {{ {src}.n }}")
            holds.append(f"  hold a{i} >= 0")

        # forward, possibly guarded (sometimes with the WRONG guard on purpose)
        if i + 1 < nproc:
            wh = ""
            pick = r.random()
            if cguard and pick < 0.45:
                wh = f" when {cguard}"
            elif pick < 0.6:
                wh = f" when {src}.n > 0"
            bp = r.choice(["", " @shed", " @deadline(5.ms)"])
            body.append(f"    send {src} to P{i+1}{bp}{wh}")
        body.append("  }")
        body.append("}")
        L.append("\n".join(body))

        holds.append(f"  hold c{i} <= k{i}")

    for i in range(nproc - 1):
        holds.append(f"  hold P{i+1}.c{i+1} <= P{i}.c{i}")
        holds.append(f"  hold P{i+1}.k{i+1} <= P{i}.k{i}")

    r.shuffle(holds)
    L.append("spec S {\n" + "\n".join(holds[: r.randint(1, len(holds))]) + "\n}")
    return "\n\n".join(L) + "\n"

if __name__ == "__main__":
    sys.stdout.write(gen(int(sys.argv[1])))
