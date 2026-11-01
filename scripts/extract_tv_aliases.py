#!/usr/bin/env python3
"""Writes data/tv_title_aliases.tsv from a newznab-tmux checkout. Reads text only. GPLv3, see README credits."""
import pathlib, re, sys
lines = (pathlib.Path(sys.argv[1]) / "app/Services/TvProcessing/Providers/AbstractTvProvider.php").read_text(encoding="utf-8").splitlines()[1189:1244]
rows = re.findall(r"'([^']+)' => '([^']+)'", "\n".join(lines))
assert len(rows) > 30
pathlib.Path("data/tv_title_aliases.tsv").write_text("".join(f"{a}\t{b}\n" for a, b in rows), encoding="utf-8")
