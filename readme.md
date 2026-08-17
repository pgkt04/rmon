rmon

Terminal system monitor with disk benchmarking.

![rmon](media/demo.png)

Panels for cpu, gpu, network, disks, processes, and memory. Process
tree, per-process threads, live filtering, kill, mouse support.
Network and disk rows hide when idle and appear when they see
traffic.

The disk benchmark measures sequential read/write MB/s and random 4k
iops with latency percentiles. Results append to
~/.rmon/bench_history.jsonl.

Runs on Linux and macOS.

usage:

    rmon
    rmon bench [--path DIR | --device DEV] [--size-mb N] [--secs N]

keys:

    q           quit
    c/m/i/n     sort procs
    f or /      filter procs
    t           threads of selected process
    e           process tree
    h           show idle interfaces/disks
    k           kill selected process
    s           system info
    + / -       refresh speed
    b           disk benchmark
