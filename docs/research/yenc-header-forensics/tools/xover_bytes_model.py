import math, random, statistics
# Header model: bytes of each header line incl CRLF (wire). Visible in XOVER: Subject, From, Message-ID, Date. Newsgroups via Xref only.
def hdr_len(name, value, crlf=2):
    return len(name) + 2 + len(value) + crlf
def nyuu_headers(fname, part, parts, total_files, fileno, frm, groups, crlf=2):
    subj = f'[{fileno:02d}/{total_files:02d}] - "{fname}" yEnc ({part}/{parts}) {{FILESIZE}}'
    subj = subj.replace('{FILESIZE}', '1234567')  # filesize digits vary, treat as const here
    mid = '<' + 'x'*24 + '-' + '1'*14 + '@nyuu>'  # 47 chars total per brief
    date = 'Mon, 02 Nov 2026 08:57:05 GMT'  # 29 chars
    h = [('Subject', subj), ('From', frm), ('Newsgroups', groups), ('Message-ID', mid), ('Date', date)]
    return sum(hdr_len(n, v, crlf) for n, v in h) + crlf  # + blank line
# Entropy estimate: quantized Gaussian over integer bins of width w
def H_quant(sd, w):
    # differential entropy of N(0,sd) minus log2(w), valid for sd >> w
    return 0.5*math.log2(2*math.pi*math.e*sd*sd) - math.log2(w)
sd_bytes = 170.0
print("Nyuu header len example:", nyuu_headers('movie.part1.rar', 12, 300, 1, 1, 'poster <p@h.example>', 'alt.binaries.test', 2))
print("H(bytes, 1B bins) bits/part ~ %.2f" % H_quant(sd_bytes, 1))
print("H(lines, 128B bins) bits/part ~ %.2f" % H_quant(sd_bytes, 128))
# header-length sd contributed by unknowns (per-server unknown: User-Agent/Path/X-*) and filename-length/part digits
for unk_sd in [0, 5, 20]:
    tot = math.sqrt(sd_bytes**2 + unk_sd**2)
    print("unknown header sd=%d -> body+hdr sd %.1f, H_bytes %.2f" % (unk_sd, tot, H_quant(tot,1)))
