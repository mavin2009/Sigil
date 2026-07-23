"""Grammar-directed fuzzer for sigilc.

Property under test: the compiler NEVER panics. For any input — valid,
malformed, truncated, deeply nested, or byte-mutated — it must either
succeed or return a clean diagnostic. A panic (exit 101) is a bug.
"""
import random, sys

TYPES = ["Int", "Float", "String", "Bool", "UUID", "Bytes", "Duration"]
TAGS = ["@error", "@recover(with: {pure})", "@timeout(50.ms) @recover(with: {pure})",
        "@timeout(30.ms) @retry(2) @recover(with: {pure})", "@timeout(20.ms) @retry(1) @error"]
ROUTES = ["", " by {key}", " broadcast"]
BP = ["", " @block", " @shed", " @deadline(5.ms)"]
WHEN = ["", " when {cond}"]

def rand_expr(r, depth, names):
    if depth <= 0 or not names:
        return r.choice([str(r.randint(-5, 100)), f"{r.uniform(-5,100):.1f}", "true", "false", '"s"'])
    k = r.randint(0, 5)
    if k == 0: return r.choice(names)
    if k == 1: return f"({rand_expr(r, depth-1, names)} + {rand_expr(r, depth-1, names)})"
    if k == 2: return f"if {rand_expr(r, depth-1, names)} > 0 {{ {rand_expr(r, depth-1, names)} }} else {{ {rand_expr(r, depth-1, names)} }}"
    if k == 3: return f"({rand_expr(r, depth-1, names)} * {rand_expr(r, depth-1, names)})"
    return r.choice(names)

def gen_program(seed):
    r = random.Random(seed)
    n_schema = r.randint(1, 3)
    schemas = []
    for i in range(n_schema):
        nf = r.randint(1, 4)
        fields = ", ".join(f"f{j}: {r.choice(TYPES)}" for j in range(nf))
        schemas.append((f"S{i}", fields))
    out = [f"schema {n} {{ {f} }}" for n, f in schemas]

    ext, pure = [], []
    for i in range(r.randint(1, 3)):
        a, b = r.choice(schemas)[0], r.choice(schemas)[0]
        out.append(f"transform e{i}(x: {a}) -> {b} {{}}")
        ext.append((f"e{i}", a, b))
    for i in range(r.randint(1, 3)):
        a = r.choice(schemas)[0]
        out.append(f"transform p{i}(x: {a}) -> {a} {{ x }}")
        pure.append((f"p{i}", a))

    n_proc = r.randint(1, 3)
    procs = [f"P{i}" for i in range(n_proc)]
    for i, pname in enumerate(procs):
        sname = r.choice(schemas)[0]
        body = [f"process {pname} {{", f"  state c{i}: Int = 0", f"  state v{i}: Float = 0.0"]
        body.append(f"  on m: {sname} {{")
        names = ["m", f"c{i}", f"v{i}"]
        if ext and r.random() < 0.7:
            e = r.choice(ext)
            pu = r.choice(pure)[0] if pure else "p0"
            tag = r.choice(TAGS).format(pure=pu)
            body.append(f"    let z = m ~> {e[0]} {tag}")
            names.append("z")
        body.append(f"    c{i} := c{i} + {rand_expr(r, 2, [f'c{i}'])}")
        if r.random() < 0.5 and i + 1 < n_proc:
            route = r.choice(ROUTES).format(key="m.f0")
            bp = r.choice(BP)
            wh = r.choice(WHEN).format(cond=f"c{i} > 0")
            body.append(f"    send m to P{i+1}{route}{bp}{wh}")
        body.append("  }")
        body.append("}")
        out.append("\n".join(body))

    if r.random() < 0.6:
        out.append("spec Sp {\n  require path_timeout_sum <= 500.ms\n  hold c0 >= 0\n}")
    return "\n\n".join(out) + "\n"

def mutate(src, r):
    b = bytearray(src.encode())
    if not b: return src
    for _ in range(r.randint(1, 6)):
        op = r.randint(0, 3)
        i = r.randrange(len(b))
        if op == 0: b[i] = r.randint(32, 126)
        elif op == 1: del b[i]
        elif op == 2: b.insert(i, r.randint(32, 126))
        else:
            j = min(len(b), i + r.randint(1, 40)); del b[i:j]
        if not b: break
    return b.decode("utf-8", errors="ignore")

if __name__ == "__main__":
    mode, seed = sys.argv[1], int(sys.argv[2])
    r = random.Random(seed)
    if mode == "valid":
        sys.stdout.write(gen_program(seed))
    elif mode == "mutated":
        sys.stdout.write(mutate(gen_program(seed), r))
    elif mode == "nested":
        d = r.randint(50, 400)
        sys.stdout.write("schema S { f0: Int }\nprocess P {\n  state c: Int = 0\n  on m: S {\n    c := " + "(" * d + "1" + ")" * d + "\n  }\n}\n")
    elif mode == "truncated":
        s = gen_program(seed); sys.stdout.write(s[: r.randrange(1, max(2, len(s)))])
