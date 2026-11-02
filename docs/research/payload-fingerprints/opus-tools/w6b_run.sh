#!/bin/bash
# usage: run.sh U L V ns(0/1) rarbin  -> prints sizes
U=$1; L=$2; V=$3; NS=$4; R=${5:-/tmp/w5a/bin/rar}
D=$(mktemp -d /tmp/w6b/d/r.XXXX); cd $D
n=$(python3 -I -c "print(('a'*$L)[:$L-4]+'.mkv')")
head -c $U /dev/urandom > "$n"
if [ $NS = 0 ]; then touch -d '2020-01-01 00:00:00' "$n"; else touch -d '2020-01-01 00:00:00.123456789' "$n"; fi
$R a -m0 -hpx -v${V}b -ep1 -idq o.rar "$n" >/dev/null 2>&1
ls o*.rar | sort -V | xargs stat -c %s | tr '\n' ' '; echo
cd /; rm -rf $D
