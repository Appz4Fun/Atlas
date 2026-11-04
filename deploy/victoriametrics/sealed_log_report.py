#!/usr/bin/env python3
"""Numbers for the sealed log, from VictoriaMetrics or Prometheus, as a Markdown table.

    sealed_log_report.py URL [WINDOW]

URL is the query endpoint, such as http://192.168.1.93:8428. WINDOW is how far back
to average, in PromQL's form (default 1h). It asks the same things as the
"Atlas: Sealed log" dashboard (deploy/grafana/dashboards.py): throughput, latency
on both ends of the wire, the disk, and failures. A row with no data prints "-",
which usually means the service or Atlas wasn't running the metrics build yet,
or nothing was offloaded in the window.
"""

import json
import sys
import urllib.error
import urllib.parse
import urllib.request

SVC = 'job="sealed-log"'
CLIENT = 'function=~"sealed_log.(fetch|verify|append)"'


def rows(w):
    def rate(expr):
        return f"sum(rate({expr}[{w}]))"

    def quantile(p, metric, sel, by=""):
        return f"histogram_quantile({p}, sum by (le{by}) (rate({metric}_bucket{{{sel}}}[{w}])))"

    def mean(metric, sel):
        return f"sum(rate({metric}_sum{{{sel}}}[{w}])) / sum(rate({metric}_count{{{sel}}}[{w}]))"

    def increase(expr):
        return f"(sum(increase({expr}[{w}])) or vector(0))"

    out = [
        ("Service", "Service up", f"min(up{{{SVC}}})", "flag"),
        ("Service", "Log size", f"sealed_log_bytes{{{SVC}}}", "bytes"),
        ("Service", "Disk available", f'sealed_log_filesystem_bytes{{{SVC},kind="avail"}}', "bytes"),
        ("Service", "Days until the disk fills",
         f'sealed_log_filesystem_bytes{{{SVC},kind="avail"}} / ignoring(kind) (deriv(sealed_log_bytes{{{SVC}}}[{w}]) * 86400) > 0', "days"),
        ("Throughput", "Written", rate(f"sealed_log_bytes_appended_total{{{SVC}}}"), "Bps"),
        ("Throughput", "Read", rate(f"sealed_log_bytes_read_total{{{SVC}}}"), "Bps"),
        ("Throughput", "Values written", rate(f"sealed_log_values_appended_total{{{SVC}}}"), "per s"),
        ("Throughput", "Values read", rate(f"sealed_log_values_read_total{{{SVC}}}"), "per s"),
        ("Throughput", "Network in at the service", rate(f'sealed_log_network_bytes_total{{{SVC},direction="received"}}'), "Bps"),
        ("Throughput", "Network out at the service", rate(f'sealed_log_network_bytes_total{{{SVC},direction="sent"}}'), "Bps"),
        ("Throughput", "Offload sent", rate('atlas_sealed_log_offload_bytes_total{kind="sent"}'), "Bps"),
        ("Throughput", "Offload freed from shards", rate('atlas_sealed_log_offload_bytes_total{kind="freed"}'), "Bps"),
        ("Throughput", "Files moved", rate('atlas_sealed_log_offload_files_total{result="moved"}') + " * 60", "per min"),
        ("Throughput", "Group commits", rate(f"sealed_log_commits_total{{{SVC}}}"), "per s"),
        ("Throughput", "Appends per commit",
         f"{rate(f'sealed_log_commit_appends_total{{{SVC}}}')} / {rate(f'sealed_log_commits_total{{{SVC}}}')}", "x"),
        ("Throughput", "Share of time throttled", f"{rate('atlas_sealed_log_offload_throttled_milliseconds_total')} / 1000", "pct"),
    ]
    for op in ("append", "read"):
        for p in (0.5, 0.95, 0.99):
            out.append(("Service latency", f"{op} p{int(p * 100)}",
                        quantile(p, "sealed_log_request_duration_seconds", f'{SVC},op="{op}"'), "s"))
    for p in (0.5, 0.95, 0.99):
        out.append(("Disk", f"fdatasync p{int(p * 100)}", quantile(p, "sealed_log_fdatasync_duration_seconds", SVC), "s"))
    out += [
        ("Disk", "Read one value p95", quantile(0.95, "sealed_log_disk_read_duration_seconds", SVC), "s"),
        ("Disk", "Waiting for the writer p95", quantile(0.95, "sealed_log_queue_duration_seconds", SVC), "s"),
    ]
    for function in ("fetch", "append", "verify"):
        for p in (0.5, 0.95):
            out.append(("Atlas waits", f"sealed_log.{function} p{int(p * 100)}",
                        quantile(p, "atlas_function_duration_seconds", f'function="sealed_log.{function}"'), "s"))
    for label, functions, op in (("append", "sealed_log.append", "append"), ("read", "sealed_log.(fetch|verify)", "read")):
        waits = mean("atlas_function_duration_seconds", f'function=~"{functions}"')
        spends = mean("sealed_log_request_duration_seconds", f'{SVC},op="{op}"')
        out.append(("Network overhead (mean)", label, f"({waits}) - ({spends})", "s"))
    out += [
        ("Failures", "Service request errors", increase(f"sealed_log_request_errors_total{{{SVC}}}"), "count"),
        ("Failures", "Commit failures", increase(f"sealed_log_commit_failures_total{{{SVC}}}"), "count"),
        ("Failures", "Checksum failures", increase(f"sealed_log_checksum_failures_total{{{SVC}}}"), "count"),
        ("Failures", "Offload batch failures", increase("atlas_sealed_log_offload_failures_total"), "count"),
        ("Failures", "Failed connects from Atlas", increase('atlas_sealed_log_connects_total{result="failed"}'), "count"),
    ]
    return out


def fmt(value, unit):
    if value is None:
        return "-"
    if unit == "flag":
        return "yes" if value >= 1 else "no"
    if unit in ("bytes", "Bps"):
        n = value
        for name in ("B", "KB", "MB", "GB", "TB", "PB"):
            if abs(n) < 1024 or name == "PB":
                return f"{n:.1f} {name}" + ("/s" if unit == "Bps" else "")
            n /= 1024
    if unit == "s":
        return f"{value * 1000:.2f} ms" if abs(value) < 1 else f"{value:.2f} s"
    if unit == "pct":
        return f"{value * 100:.1f}%"
    if unit == "days":
        return f"{value:.0f} days"
    if unit == "count":
        return f"{value:.0f}"
    if unit == "x":
        return f"{value:.1f}"
    return f"{value:.1f} {unit}"


def query(url, expr):
    """The first sample's value, or None when the answer is empty or not a number."""
    full = url.rstrip("/") + "/api/v1/query?" + urllib.parse.urlencode({"query": expr})
    with urllib.request.urlopen(full, timeout=30) as r:
        body = json.load(r)
    if body.get("status") != "success":
        raise RuntimeError(f"{expr}: {body.get('error', body)}")
    result = body["data"]["result"]
    if not result:
        return None
    value = float(result[0]["value"][1])
    return None if value != value or value in (float("inf"), float("-inf")) else value


def report(url, window, ask=query):
    lines = [f"Sealed log over the last {window}", "", "| Section | Measure | Value |", "|---|---|---|"]
    for section, label, expr, unit in rows(window):
        lines.append(f"| {section} | {label} | {fmt(ask(url, expr), unit)} |")
    return "\n".join(lines)


def main(argv):
    if len(argv) not in (2, 3) or argv[1] in ("-h", "--help"):
        sys.exit(__doc__)
    try:
        print(report(argv[1], argv[2] if len(argv) == 3 else "1h"))
    except (urllib.error.URLError, RuntimeError) as e:
        sys.exit(f"sealed_log_report: {e}")


if __name__ == "__main__":
    main(sys.argv)
