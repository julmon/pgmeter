# pgmeter system sampler (protocol 1). Runs under `sh -s -- <interval>`,
# locally or at the far end of an SSH connection, and copies /proc to stdout
# every <interval> seconds. It computes nothing: pgmeter parses the text.
#
#   @hello <version>      first line pgmeter looks at; anything before it
#                         (a login banner) is ignored
#   @host, @kernel, @addrs, @blockdevs, @stack, @dmname, @holds
#                         once, before the first sample
#   @sample ... @end      one sample: every line between the two is
#                         `<path>:<line of that file>`, as grep prints them
#   @error <message>      the sampler cannot run here
INTERVAL=${1:-1}

os=$(uname -s)
if [ "$os" != Linux ]; then
    echo "@error unsupported operating system: $os"
    exit 1
fi
if [ ! -r /proc/stat ]; then
    echo "@error /proc/stat is not readable"
    exit 1
fi

FILES="/proc/uptime /proc/stat /proc/meminfo /proc/vmstat /proc/diskstats /proc/loadavg"
# Pressure stall information: Linux 4.20 or later, and only when enabled.
for resource in cpu memory io; do
    if [ -r "/proc/pressure/$resource" ]; then
        FILES="$FILES /proc/pressure/$resource"
    fi
done

echo "@hello 1"
echo "@host $(uname -n)"
echo "@kernel $(uname -r)"
# The host's addresses, to tell which monitored host PostgreSQL runs on.
echo "@addrs $( (hostname -I || ip -o addr | awk '{sub(/\/.*/, "", $4); print $4}') 2>/dev/null | tr '\n' ' ')"
# Whole block devices, to tell them from their partitions in /proc/diskstats.
echo "@blockdevs $(ls /sys/block 2>/dev/null | tr '\n' ' ')"
# How the devices stack: `@stack <device> <the whole devices under it>` for a
# device-mapper or RAID volume, and `@dmname <device> <its mapper name>`.
for dev in /sys/block/*; do
    [ -d "$dev" ] || continue
    name=${dev##*/}
    if [ -r "$dev/dm/name" ]; then
        echo "@dmname $name $(cat "$dev/dm/name")"
    fi
    under=
    for slave in "$dev"/slaves/*; do
        [ -e "$slave" ] || continue
        below=${slave##*/}
        # A partition counts as its whole device.
        for whole in /sys/block/*/"$below"; do
            if [ -e "$whole" ]; then
                below=${whole%/*}
                below=${below##*/}
            fi
        done
        under="$under $below"
    done
    if [ -n "$under" ]; then
        echo "@stack $name$under"
    fi
done

# `@holds <device> <what>`: the whole device under a path PostgreSQL uses.
# pgmeter puts a `pgm_paths` function ahead of this script when it knows the
# paths; each of its lines is `holds <what> <path>`.
holds() {
    fs=$(df -P "$2" 2>/dev/null | awk 'NR == 2 { print $1 }')
    case $fs in
        /dev/*) ;;
        # No such path here, or a file system without one block device.
        *) return 0 ;;
    esac
    # /dev/mapper/vg-data is a link to /dev/dm-3.
    node=$(readlink -f "$fs" 2>/dev/null) || node=$fs
    below=${node##*/}
    for whole in /sys/block/*/"$below"; do
        if [ -e "$whole" ]; then
            below=${whole%/*}
            below=${below##*/}
        fi
    done
    echo "@holds $below $1"
}
if command -v pgm_paths >/dev/null 2>&1; then
    pgm_paths
fi

while :; do
    echo "@sample"
    # One process reads every file. The `intr` line of /proc/stat has one
    # counter per interrupt line, tens of kilobytes on a large machine.
    grep -v '^intr' $FILES 2>/dev/null
    # The reader is gone when this write fails.
    echo "@end" || exit 0
    # Detached from the pipes, so that they close as soon as this shell is
    # killed and not when its last sleep ends.
    sleep "$INTERVAL" </dev/null >/dev/null 2>&1
done
