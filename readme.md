rmon

Terminal system monitor with disk benchmarking.

Written in Rust. Panels for cpu (usage, name, temperature, gpu),
network, disks (per-disk io, mounts, smart health), processes, and
memory (used, available, swap).

The process panel sorts by cpu, memory, io, or name, filters live as
you type, shows per-process threads, and lays processes out as a
parent/child tree. Selected rows stay pinned on screen while the list
resorts. Full mouse support: wheel scroll, click to select, draggable
scrollbar. Selected processes can be sent SIGTERM after a confirmation
prompt.

The disk benchmark measures sequential read/write MB/s and random 4k
iops with p50/p99 latency, using direct io where the os allows it.
Results append to ~/.rmon/bench_history.jsonl. A read-only raw device
test is available behind --device.

Runs on Linux and macOS.

usage:

    rmon
    rmon bench [--path DIR | --device DEV] [--size-mb N] [--secs N]

keys:

    q           quit
    up/down     move selection
    c/m/i/n     sort procs by cpu, memory, io, name
    f or /      filter procs (enter keeps it, esc clears it)
    t           show threads under each process
    e           process tree view
    k           kill selected process (y/enter confirms)
    b           run disk benchmark
    mouse       wheel scrolls, click selects, scrollbar drags
