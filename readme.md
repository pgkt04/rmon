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

install:

    curl -fsSL https://raw.githubusercontent.com/pgkt04/rmon/main/install.sh | sh

The Linux binaries are static, so they run on any distro regardless of
glibc version. Builds exist for linux x86_64, linux aarch64, macos arm64,
and macos x86_64. Set `RMON_BIN_DIR` to choose the install directory and
`RMON_VERSION` to pin a release.

Or grab a tarball from the [releases
page](https://github.com/pgkt04/rmon/releases), or build from source:

    cargo install --git https://github.com/pgkt04/rmon

update an existing install (or just re-run the install one-liner):

    rmon update

usage:

    rmon
    rmon bench [--path DIR | --device DEV] [--size-mb N] [--secs N]
    rmon update
    rmon --help

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
