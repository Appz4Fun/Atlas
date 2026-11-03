#!/bin/bash
# w8a: real -hp / 7z -mhe runs: same file, different random passwords/lengths/names
R=/tmp/w5a/bin/rar; Z=/tmp/w6a/d/xp/usr/lib/p7zip/7za
D=/tmp/w8a/d; cd $D; rm -rf runs; mkdir runs; cd runs
head -c 9000123 /dev/urandom > src.bin; touch -d @1700000000 src.bin
for nm in Movie.2024.1080p.WEB.x264-GRP.mkv Movie.2024.1080p.WEB.x264-XYZ.mkv Movie.2024.1080p.WEB.x264-GRPx.mkv Movie.2024.1080p.WEB.x264-GRPxxxxxxxxxxxxxxxxx.mkv; do
  cp -p src.bin $nm
  for pw in $(head -c 12 /dev/urandom|od -An -tx1|tr -d " \n") $(head -c 40 /dev/urandom|od -An -tx1|tr -d " \n") a; do
    o=r_${#nm}_${#pw}_$RANDOM; mkdir $o
    $R a -idq -m0 -ma5 -hp$pw -v2000000b -ep1 $o/x.rar $nm >/dev/null
    echo "rar L=${#nm} pwlen=${#pw} $(stat -c %s $o/* | tr '\n' ' ')"
    rm -rf $o
    o=z_$RANDOM.7z; $Z a -bd -p$pw -mhe=on -mx0 $o $nm >/dev/null; echo "7z  L=${#nm} pwlen=${#pw} $(stat -c %s $o)"; rm $o
    o=z9_$RANDOM.7z; $Z a -bd -p$pw -mhe=on -mx5 $o $nm >/dev/null; echo "7zmx5 L=${#nm} pwlen=${#pw} $(stat -c %s $o)"; rm $o
  done; rm $nm
done
