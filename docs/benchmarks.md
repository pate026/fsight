# Benchmarks

Machine: Intel Core i3-3110M (2 cores / 4 threads), 7.6 GiB RAM, WD 320 GB
**HDD**, btrfs on LUKS, Fedora. Tools: GNU du 9.10, GNU find 4.10.0, GNU grep
3.12, fd 10.5.2, ripgrep 15.2.0, fsight 0.1.0 (release build, 4 threads unless
noted). Script: [benchmark.py](benchmark.py) (one warm-up run, then the median
of 5 runs).

**Warm cache only.** Dropping the page cache needs root, so these numbers
measure CPU and system-call efficiency, not the disk. On a cold HDD, seek time
dominates everything.

| Task | Tool | Median | Peak RSS |
|---|---|---:|---:|
| Disk usage of /usr (180k entries), total | `du -sx` | 0.62 s | 3 MiB |
| | `fsight scan -d 0` | **0.43 s** | 11 MiB |
| | `fsight scan -d 0 -j 1` | 1.11 s | 8 MiB |
| Disk usage, 3 levels | `du -x --max-depth=3` | 0.64 s | 3 MiB |
| | `fsight scan -d 3 -n 0` | **0.50 s** | 13 MiB |
| Find `*.h` in /usr (1 689 results) | GNU `find -iname` | 0.53 s | 4 MiB |
| | `fd -uu -g` | 0.19 s | 13 MiB |
| | `fsight find` | **0.19 s** | 11 MiB |
| List all of /usr (179 990 results) | GNU `find` | 0.41 s | 4 MiB |
| | `fd -uu` | **0.21 s** | 15 MiB |
| | `fsight find '*'` | 0.46 s | 35 MiB |
| Grep `TODO\|FIXME` in /usr/include (13 MB) | GNU `grep -rIEn` | 0.039 s | 3 MiB |
| | `rg -uu` | **0.018 s** | 7 MiB |
| | `fsight grep` | 0.043 s | 7 MiB |
| Grep `-i license` in /usr/share/doc (157 MB) | GNU `grep -rIin` | 0.29 s | 3 MiB |
| | `rg -uu -i` | **0.10 s** | 8 MiB |
| | `fsight grep -i` | 0.31 s | 8 MiB |
| | `fsight grep -i -j 1` | 0.59 s | 8 MiB |

All tools found the same results: the same 1 689 `.h` files and 179 990
entries, and 1 858 files / 17 382 matching lines for `license`.

## What this shows (honestly)

* **scan beats du only thanks to threads.** fsight is about 1.5× faster than
  `du` with 4 threads, but 1.8× *slower* single-threaded. It does more work per
  entry: the `ignore` walker, a channel send, building the tree.
* **find by pattern is as fast as fd**, and about 2.8× faster than
  single-threaded GNU find.
* **Listing huge result sets is fsight's weak spot.** It is slower than GNU find
  and over 2× slower than fd, and uses more memory, because it collects and
  sorts all results before printing (decision D27) and allocates a `String` per
  result. Two fixes made while benchmarking brought it from 0.67 s down to
  0.46 s: a cheaper path comparison for sorting, and skipping the highlight
  work when colors are off.
* **grep is on par with GNU grep but about 3× slower than ripgrep.** fsight
  calls the regex once per line, while ripgrep searches whole buffers with
  SIMD literal prefilters and only splits lines around matches. Threads give a
  1.9× speedup on this 2-core CPU.

Ideas to close the gaps: an unsorted streaming mode for `find`, buffer-wide
search in `grep`, and fewer allocations per result.
