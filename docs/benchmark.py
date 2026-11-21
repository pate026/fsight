# Benchmark harness for docs/benchmarks.md: one warm-up run, then N timed runs
# (median and minimum), peak memory via /usr/bin/time, and the number of output
# lines (to check that the tools did the same work).
#
# Usage (from the repository root, after `cargo build --release`):
#     python3 docs/benchmark.py results.json
import json, os, statistics, subprocess, sys, time

FS = os.environ.get("FSIGHT", "target/release/fsight")
RUNS = 5
CASES = [
    ("Disk usage of /usr (total only)", [
        ("du -sx", ["/usr/bin/du", "-sx", "/usr"]),
        ("fsight scan -d 0 (4 threads)", [FS, "scan", "/usr", "-d", "0", "--hidden", "--no-ignore", "-x", "-q"]),
        ("fsight scan -d 0 -j 1", [FS, "scan", "/usr", "-d", "0", "--hidden", "--no-ignore", "-x", "-q", "-j", "1"]),
    ]),
    ("Disk usage of /usr, tree 3 levels deep", [
        ("du -x --max-depth=3", ["/usr/bin/du", "-x", "--max-depth=3", "/usr"]),
        ("fsight scan -d 3", [FS, "scan", "/usr", "-d", "3", "-n", "0", "--hidden", "--no-ignore", "-x", "-q", "--no-color"]),
    ]),
    ("Find *.h under /usr (case-insensitive)", [
        ("GNU find -iname", ["/usr/bin/find", "/usr", "-iname", "*.h"]),
        ("fd -uu -g", ["fd", "-uu", "-g", "*.h", "/usr"]),
        ("fsight find", [FS, "find", "*.h", "/usr", "--hidden", "--no-ignore", "-q", "--no-color"]),
    ]),
    ("List every entry under /usr", [
        ("GNU find", ["/usr/bin/find", "/usr", "-mindepth", "1"]),
        ("fd -uu", ["fd", "-uu", ".", "/usr"]),
        ("fsight find '*'", [FS, "find", "*", "/usr", "--hidden", "--no-ignore", "-q", "--no-color"]),
    ]),
    ("Grep 'TODO|FIXME' in /usr/include (13 MB of headers)", [
        ("GNU grep -rIEn", ["/usr/bin/grep", "-rIEn", "TODO|FIXME", "/usr/include"]),
        ("rg -uu -n", ["rg", "-uu", "-n", "--no-heading", "TODO|FIXME", "/usr/include"]),
        ("fsight grep", [FS, "grep", "TODO|FIXME", "/usr/include", "--hidden", "--no-ignore", "-q", "--no-color"]),
    ]),
    ("Grep -i 'license' in /usr/share/doc (157 MB, 5.7k files)", [
        ("GNU grep -rIin", ["/usr/bin/grep", "-rIin", "license", "/usr/share/doc"]),
        ("rg -uu -i -n", ["rg", "-uu", "-i", "-n", "--no-heading", "license", "/usr/share/doc"]),
        ("fsight grep -i", [FS, "grep", "-i", "license", "/usr/share/doc", "--hidden", "--no-ignore", "-q", "--no-color"]),
        ("fsight grep -i -j 1", [FS, "grep", "-i", "license", "/usr/share/doc", "--hidden", "--no-ignore", "-q", "--no-color", "-j", "1"]),
    ]),
]

def timed(argv):
    start = time.perf_counter()
    subprocess.run(argv, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    return time.perf_counter() - start

def output_lines(argv):
    out = subprocess.run(argv, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL).stdout
    return out.count(b"\n")

def peak_rss_mib(argv):
    r = subprocess.run(["/usr/bin/time", "-f", "%M"] + argv, stdout=subprocess.DEVNULL, stderr=subprocess.PIPE)
    return int(r.stderr.strip().splitlines()[-1]) / 1024

results = []
for title, commands in CASES:
    print(f"\n## {title}", flush=True)
    for label, argv in commands:
        timed(argv)  # warm-up (fills the page cache)
        times = sorted(timed(argv) for _ in range(RUNS))
        lines = output_lines(argv)
        rss = peak_rss_mib(argv)
        row = {"case": title, "tool": label, "median": statistics.median(times), "min": times[0], "lines": lines, "rss_mib": rss}
        results.append(row)
        print(f"  {label:32} median {row['median']:6.3f}s  min {row['min']:6.3f}s  peak RSS {rss:7.1f} MiB  output {lines} lines", flush=True)
json.dump(results, open(sys.argv[1], "w"), indent=1)
