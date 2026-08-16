rmon

Terminal system monitor with disk benchmarking.

A btop-style monitor written in Rust. Panels for cpu (usage, name,
temperature, gpu), network, disks (per-disk io, mounts, smart health),
processes (sortable, mouse scroll), and memory (used, available, swap).

The disk benchmark measures sequential read/write MB/s and random 4k
iops with p50/p99 latency, using direct io where the os allows it.
Results append to ~/.rmon/bench_history.jsonl. A read-only raw device
test is available behind --device.

Runs on Linux and macOS.

usage:

    rmon
    rmon bench [--path DIR | --device DEV] [--size-mb N] [--secs N]

keys: q quit, c/m/i sort procs, b run disk bench, mouse wheel to scroll
