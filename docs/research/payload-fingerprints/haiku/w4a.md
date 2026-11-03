# W4A mtime by acquisition pipeline (Haiku, ~08:27Z)
- SABnzbd ignore_unrar_dates -> unrar -tsm- -> extraction time; default keeps archive mtime; forum says switch not fully reliable. sabnzbd newsunpack.py; forums.sabnzbd.org p=122657
- WinRAR: stores mtime by default (RAR4 2 s, RAR5 1 s, high precision optional), restores on extract. techshelps WinRAR HELPGetArcTime
- BEP 47: no per-file mtime in torrents -> torrent-sourced files get client write/completion time (rTorrent libtorrent resume data stores mtime, inference). bittorrent.org bep_0047
- rclone keeps modtime on capable backends (chain preserves origin mtime). rclone.org/commands/rclone_copy
- yEnc has no time field: raw-posted (non-archived) files give no mtime; only archive headers carry it.
- Unverified: 7z -mtm default (believed on), NZBGet unpack times, ngPost/Nyuu touching times, scene pre vs mtime.
Interpretation (Proves = origin type, Cost = free from header): inner mtime that equals another post's = same source copy; mtime many hours after scene pre / clustered by uploader = P2P/download-time origin; round 2 s values = RAR4 writer.
