#!/usr/bin/env python3
"""Atlas's Grafana dashboards, as code.

    dashboards.py build             write dashboards/*.json
    dashboards.py push URL          create the Atlas folder and import them
                                    (GRAFANA_USER / GRAFANA_PASSWORD, default admin / admin;
                                    GRAFANA_DATASOURCE, the data source uid, default victoriametrics)
"""

import base64
import json
import os
import pathlib
import sys
import urllib.error
import urllib.request

HERE = pathlib.Path(__file__).parent
DATASOURCE = {"type": "prometheus", "uid": os.environ.get("GRAFANA_DATASOURCE", "victoriametrics")}
FOLDER = {"uid": "atlas", "title": "Atlas"}

# the failures that say something is wrong: a missing article or group is the normal answer
REAL_FAILURES = 'kind!~"no_such_.*"'
SIZE_BOUNDS = [2**n * 1048576 for n in range(21)]


def size_name(bytes_):
    return f"{bytes_ // 2**30} GB" if bytes_ >= 2**30 else f"{bytes_ // 2**20} MB"


class Board:
    def __init__(self, uid, title, description, variables=()):
        self.uid, self.title, self.description = uid, title, description
        self.variables = list(variables)
        self.panels = []
        self.x = self.y = 0
        self.row_height = 0
        self.next_id = 1

    def _place(self, w, h):
        if self.x + w > 24:
            self.x = 0
            self.y += self.row_height
            self.row_height = 0
        at = {"h": h, "w": w, "x": self.x, "y": self.y}
        self.x += w
        self.row_height = max(self.row_height, h)
        return at

    def row(self, title):
        self.x = 0
        self.y += self.row_height
        self.row_height = 0
        self.panels.append(
            {"id": self._id(), "type": "row", "title": title, "collapsed": False, "panels": [],
             "gridPos": {"h": 1, "w": 24, "x": 0, "y": self.y}}
        )
        self.y += 1

    def _id(self):
        self.next_id += 1
        return self.next_id

    def panel(self, kind, title, targets, w, h, description="", unit=None, options=None, field=None, overrides=(),
              transformations=(), links=()):
        defaults = dict(field or {})
        if unit:
            defaults["unit"] = unit
        if links:
            defaults["links"] = [{"title": t, "url": u, "targetBlank": False} for t, u in links]
        self.panels.append(
            {
                "id": self._id(),
                "type": kind,
                "title": title,
                "description": description,
                "datasource": DATASOURCE,
                "gridPos": self._place(w, h),
                "targets": [dict(t, refId=chr(65 + i), datasource=DATASOURCE) for i, t in enumerate(targets)],
                "fieldConfig": {"defaults": defaults, "overrides": list(overrides)},
                "options": options or {},
                "transformations": list(transformations),
            }
        )

    def stat(self, title, expr, unit="short", w=3, h=4, description="", thresholds=None, links=(), mappings=None):
        field = {"thresholds": {"mode": "absolute", "steps": thresholds or [{"color": "text", "value": None}]}}
        if mappings:
            field["mappings"] = mappings
        self.panel(
            "stat", title, [q(expr, instant=True)], w, h, description, unit,
            {"colorMode": "value" if not thresholds else "background", "graphMode": "none", "justifyMode": "auto",
             "reduceOptions": {"calcs": ["lastNotNull"], "fields": "", "values": False}, "textMode": "auto"},
            field, links=links,
        )

    def graph(self, title, targets, unit="short", w=12, h=8, description="", stacked=False, links=(), bars=False):
        field = {"custom": {"fillOpacity": 25 if stacked or bars else 8, "lineWidth": 1, "showPoints": "never",
                            "spanNulls": False, "drawStyle": "bars" if bars else "line",
                            "stacking": {"mode": "normal" if stacked else "none", "group": "A"}}}
        self.panel(
            "timeseries", title, targets, w, h, description, unit,
            {"legend": {"displayMode": "table", "placement": "bottom", "calcs": ["lastNotNull", "max"], "showLegend": True,
                        "sortBy": "Max", "sortDesc": True},
             "tooltip": {"mode": "multi", "sort": "desc"}},
            field, links=links,
        )

    def pie(self, title, expr, legend, unit="short", w=8, h=9, description="", links=()):
        self.panel(
            "piechart", title, [q(expr, legend, instant=True)], w, h, description, unit,
            {"pieType": "donut", "displayLabels": ["percent"], "tooltip": {"mode": "single"},
             "legend": {"displayMode": "table", "placement": "right", "values": ["value", "percent"], "showLegend": True},
             "reduceOptions": {"calcs": ["lastNotNull"], "fields": "", "values": False}},
            links=links,
        )

    def bars(self, title, targets, unit="short", w=12, h=10, description="", links=()):
        self.panel(
            "bargauge", title, targets, w, h, description, unit,
            {"displayMode": "gradient", "orientation": "horizontal", "showUnfilled": True, "valueMode": "text",
             "reduceOptions": {"calcs": ["lastNotNull"], "fields": "", "values": False}},
            {"min": 0, "color": {"mode": "continuous-BlYlRd"}}, links=links,
        )

    def table(self, title, targets, columns, w=12, h=10, description="", sort=None, links=()):
        """`columns`: label or `Value #A` -> (shown name, unit or None)"""
        overrides = []
        renames = {}
        for source, (shown, unit) in columns.items():
            renames[source] = shown
            props = []
            if unit:
                props.append({"id": "unit", "value": unit})
            if links and source == next(iter(columns)):
                props.append({"id": "links", "value": [{"title": t, "url": u} for t, u in links]})
            if props:
                overrides.append({"matcher": {"id": "byName", "options": shown}, "properties": props})
        keep = {name: True for name in columns}
        # one key column: rows meet on it, whatever other labels the queries have
        keys = [name for name in columns if not name.startswith("Value #")]
        join = ({"id": "joinByField", "options": {"byField": keys[0], "mode": "outer"}} if len(keys) == 1
                else {"id": "merge", "options": {}})
        self.panel(
            "table", title, [dict(t, format="table", instant=True, range=False) for t in targets], w, h, description, None,
            {"showHeader": True, "cellHeight": "sm",
             "sortBy": [{"displayName": sort, "desc": True}] if sort else []},
            {"custom": {"filterable": True}}, overrides,
            transformations=[
                join,
                {"id": "filterFieldsByName", "options": {"include": {"names": list(keep)}}},
                {"id": "organize", "options": {"renameByName": renames,
                                               "indexByName": {name: i for i, name in enumerate(columns)}}},
            ],
        )

    def heatmap(self, title, expr, w=12, h=9, description="", unit="bytes"):
        self.panel(
            "heatmap", title, [q(expr, "{{le}}", format="heatmap")], w, h, description, None,
            {"calculate": False, "cellGap": 1, "color": {"mode": "scheme", "scheme": "Spectral", "steps": 64, "exponent": 0.5,
                                                          "reverse": True, "scale": "exponential", "fill": "dark-orange"},
             "exemplars": {"color": "rgba(255,0,255,0.7)"}, "legend": {"show": True},
             "rowsFrame": {"layout": "auto"}, "tooltip": {"mode": "single", "yHistogram": False},
             "yAxis": {"axisPlacement": "left", "reverse": False, "unit": unit}},
        )

    def build(self):
        return {
            "uid": self.uid,
            "title": self.title,
            "description": self.description,
            "tags": ["atlas"],
            "schemaVersion": 39,
            "editable": True,
            "graphTooltip": 1,
            "refresh": "30s",
            "time": {"from": "now-6h", "to": "now"},
            "timezone": "browser",
            "links": [{"title": "Atlas", "type": "dashboards", "tags": ["atlas"], "asDropdown": False, "includeVars": False,
                       "keepTime": True, "icon": "external link", "targetBlank": False}],
            "templating": {"list": self.variables},
            "annotations": {"list": []},
            "panels": self.panels,
        }


def q(expr, legend="", instant=False, format=None):
    t = {"expr": expr, "legendFormat": legend or "__auto", "editorMode": "code", "instant": instant, "range": not instant}
    if format:
        t["format"] = format
    return t


def variable(name, label, query, multi=True, include_all=True):
    return {
        "name": name, "label": label, "type": "query", "datasource": DATASOURCE, "refresh": 2, "sort": 1,
        "query": {"query": query, "refId": "A"}, "definition": query,
        "multi": multi, "includeAll": include_all, "allValue": ".*" if include_all else None,
        "current": {"text": "All", "value": "$__all"} if include_all else {},
    }


UP_MAP = [{"type": "value", "options": {"0": {"text": "DOWN", "color": "red", "index": 0},
                                        "1": {"text": "UP", "color": "green", "index": 1}}},
          {"type": "special", "options": {"match": "null", "result": {"text": "DOWN", "color": "red", "index": 2}}}]
GOOD_BAD = [{"color": "green", "value": None}, {"color": "red", "value": 1}]

LINK_NNTP = ("NNTP servers", "/d/atlas-nntp?${__url_time_range}")
LINK_FUNCTIONS = ("Functions", "/d/atlas-functions?${__url_time_range}")
LINK_CONTENT = ("Releases and content", "/d/atlas-content?${__url_time_range}")
LINK_PIPELINE = ("Pipeline", "/d/atlas-pipeline?${__url_time_range}")
LINK_SYSTEM = ("Database and process", "/d/atlas-system?${__url_time_range}")


def overview():
    b = Board("atlas-overview", "Atlas: Overview",
              "How Atlas is doing at a glance. Every panel links to the dashboard with the detail.")
    b.row("Is it running")
    b.stat("Indexer", 'up{job="atlas",role="indexer"}', w=2, thresholds=[{"color": "text", "value": None}], mappings=UP_MAP, links=[LINK_SYSTEM])
    b.stat("API", 'up{job="atlas",role="api"}', w=2, thresholds=[{"color": "text", "value": None}], mappings=UP_MAP, links=[LINK_SYSTEM])
    b.stat("Indexer uptime", 'time() - atlas_process_start_time_seconds{role="indexer"}', "s", w=4, links=[LINK_SYSTEM])
    b.stat("NNTP connections", 'sum(atlas_nntp_connections{state="open"})', w=4, links=[LINK_NNTP],
           description="Open across every server. The NNTP dashboard has them per server.")
    b.stat("NNTP failures in range", f"sum(increase(atlas_nntp_failures_total{{{REAL_FAILURES}}}[$__range]))", w=4, links=[LINK_NNTP],
           thresholds=[{"color": "green", "value": None}, {"color": "orange", "value": 1}, {"color": "red", "value": 100}],
           description="Everything but missing articles and groups, which are ordinary answers.")
    b.stat("Bandwidth now", 'sum(rate(atlas_nntp_traffic_bytes_total{kind="wire"}[5m]))', "Bps", w=4, links=[LINK_NNTP])
    b.stat("Bandwidth in range", 'sum(increase(atlas_nntp_traffic_bytes_total{kind="wire"}[$__range]))', "bytes", w=4, links=[LINK_NNTP])
    b.stat("Indexer errors in range", "sum(increase(atlas_indexer_errors_total[$__range]))", w=12, links=[LINK_PIPELINE])
    b.stat("Stage waiting on bodies", 'atlas_stage_queue{state="net_wanted"}', w=12, links=[LINK_PIPELINE])
    b.row("What it has made")
    b.stat("Releases stored", 'atlas_db_rows{kind="releases"}', links=[LINK_CONTENT], w=4,
           description="Running total the shards keep.")
    b.stat("New releases in range", 'sum(delta(atlas_db_rows{kind="releases"}[$__range]))', links=[LINK_CONTENT], w=4)
    b.stat("Deobfuscated in range", "sum(increase(atlas_deobfuscated_total[$__range]))", links=[LINK_CONTENT], w=4,
           description="Obfuscated posts given their first real name.")
    b.stat("PreDB match rate",
           'atlas_catalog_releases{state="predb_matched"} / ignoring(state) atlas_catalog_releases{state="total"}',
           "percentunit", links=[LINK_CONTENT], w=4, description="From the last catalog walk.")
    b.stat("Average release size", 'atlas_catalog_bytes / ignoring(state) atlas_catalog_releases{state="total"}', "bytes", links=[LINK_CONTENT], w=4)
    b.stat("Catalog size", "atlas_catalog_bytes", "bytes", links=[LINK_CONTENT], w=4,
           description="What the stored releases add up to on Usenet.")
    b.graph("New releases per minute", [q('sum(deriv(atlas_db_rows{kind="releases"}[5m])) * 60', "new releases")], links=[LINK_CONTENT], w=8)
    b.graph("Deobfuscated per minute, by source", [q("sum by (source) (rate(atlas_deobfuscated_total[5m])) * 60", "{{source}}")], stacked=True, links=[LINK_CONTENT], w=8)
    b.graph("Articles indexed per second", [q("sum(rate(atlas_articles_total[5m]))", "articles")], links=[LINK_PIPELINE], w=8)
    b.pie("Releases by category", 'sum by (root) (atlas_catalog_category_releases)', "{{root}}", links=[LINK_CONTENT])
    b.pie("Where names came from", "atlas_catalog_name_source_releases", "{{source}}", links=[LINK_CONTENT])
    b.pie("PreDB matches by category", "atlas_catalog_predb_category_releases", "{{category}}", links=[LINK_CONTENT])
    b.row("How it is working")
    b.graph("NNTP connections per server", [q('atlas_nntp_connections{state="open"}', "{{server}}")], stacked=True, links=[LINK_NNTP], w=8)
    b.graph("Bandwidth per server", [q('rate(atlas_nntp_traffic_bytes_total{kind="wire"}[5m])', "{{server}}")], "Bps", stacked=True, links=[LINK_NNTP], w=8)
    b.graph("Failures per server", [q(f"sum by (server) (increase(atlas_nntp_failures_total{{{REAL_FAILURES}}}[5m]))", "{{server}}")], bars=True, links=[LINK_NNTP], w=8)
    b.graph("Time per second, top functions", [q("topk(10, sum by (function) (rate(atlas_function_duration_seconds_sum[5m])))", "{{function}}")], "s", links=[LINK_FUNCTIONS],
            description="Seconds spent per second. Over 1 means calls overlap.")
    b.graph("Bandwidth, top functions", [q("topk(10, sum by (function) (rate(atlas_function_bytes_total[5m])))", "{{function}}")], "Bps", links=[LINK_FUNCTIONS])
    return b


def content():
    root = variable("root", "Category", "label_values(atlas_catalog_release_size_count, root)")
    b = Board("atlas-content", "Atlas: Releases and content",
              "What Atlas has stored: categories, sizes, groups, release groups and the PreDB. Totals come from the catalog walk.",
              [root])
    b.row("Releases")
    b.stat("Releases", 'atlas_catalog_releases{state="total"}', w=4)
    b.stat("Complete", 'atlas_catalog_releases{state="complete"} / ignoring(state) atlas_catalog_releases{state="total"}', "percentunit", w=4)
    b.stat("NZB ready", 'atlas_catalog_releases{state="nzb_ready"}', w=4, description="At least one article saved.")
    b.stat("Named", 'atlas_catalog_releases{state="named"}', w=4, description="Carry a name found by a processing step.")
    b.stat("Still obfuscated", 'atlas_catalog_releases{state="still_obfuscated"}', w=4)
    b.stat("Last walk", "time() - atlas_catalog_scan_timestamp_seconds", "s", w=4, description="Age of the catalog numbers.")
    b.graph("New releases per minute", [q('sum(deriv(atlas_db_rows{kind="releases"}[5m])) * 60', "new releases")])
    b.graph("Releases over time", [q('atlas_catalog_releases{state=~"total|complete|named"}', "{{state}}")])
    b.row("Deobfuscation")
    b.stat("Deobfuscated in range", "sum(increase(atlas_deobfuscated_total[$__range]))", w=6)
    b.stat("Names written in range", "sum(increase(atlas_names_applied_total[$__range]))", w=6)
    b.stat("Deobfuscated share of obfuscated", 'sum(atlas_catalog_name_source_releases{source!~"posted|cleared"}) / (sum(atlas_catalog_name_source_releases{source!~"posted|cleared"}) + sum(atlas_catalog_releases{state="still_obfuscated"}))', "percentunit", w=6,
           description="Named by a step, out of those plus the ones still obfuscated.")
    b.stat("Still obfuscated", 'atlas_catalog_releases{state="still_obfuscated"}', w=6)
    b.graph("Deobfuscated per minute, by source", [q("sum by (source) (rate(atlas_deobfuscated_total[5m])) * 60", "{{source}}")], stacked=True)
    b.pie("Where names came from", "atlas_catalog_name_source_releases", "{{source}}", w=12)
    b.row("Categories")
    b.pie("Releases by root category", "sum by (root) (atlas_catalog_category_releases)", "{{root}}")
    b.pie("Size by root category", "sum by (root) (atlas_catalog_category_bytes)", "{{root}}", "bytes")
    b.table("Every category", [q("atlas_catalog_category_releases", "A"), q("atlas_catalog_category_bytes", "B")],
            {"category": ("Category", None), "Value #A": ("Releases", "short"), "Value #B": ("Size", "bytes")}, w=8, h=9, sort="Releases")
    b.graph("Filed per minute, by root", [q("sum by (root) (rate(atlas_releases_categorized_total[5m])) * 60", "{{root}}")], stacked=True,
            description="Releases the category step filed, including those looked at again after a rename or as they grow.")
    b.graph("Filed per minute, by category", [q("topk(15, sum by (category) (rate(atlas_releases_categorized_total[5m])) * 60)", "{{category}}")], stacked=True)
    b.row("Sizes")
    b.table("Average size per root", [q("atlas_catalog_release_size_sum_bytes / atlas_catalog_release_size_count", "A"), q("atlas_catalog_release_size_count", "B"), q("atlas_catalog_release_size_sum_bytes", "C")],
            {"root": ("Category", None), "Value #A": ("Average", "bytes"), "Value #B": ("Releases", "short"), "Value #C": ("Total", "bytes")}, w=8, h=10, sort="Total")
    size = 'sum(atlas_catalog_release_size_bytes{root=~"$root",le="%s"})'
    buckets = []
    prev = None
    for bound in SIZE_BOUNDS:
        expr = size % float_label(bound)
        if prev is not None:
            expr += " - " + size % float_label(prev)
        name = f"up to {size_name(bound)}" if prev is None else f"{size_name(prev)} to {size_name(bound)}"
        buckets.append(q(expr, name, instant=True))
        prev = bound
    b.bars("How big releases are now", buckets, w=8, h=10)
    b.heatmap("Heat map: catalog size distribution over time", 'sum by (le) (atlas_catalog_release_size_bytes{root=~"$root"})', w=8, h=10,
              description="How many stored releases fall in each size band, at each walk.")
    b.heatmap("Heat map: size of releases as they settle", 'sum by (le) (increase(atlas_release_size_bytes_bucket{root=~"$root"}[$__rate_interval]))', w=24, h=10,
              description="Live: the size of each release as the category step files it.")
    b.row("Biggest groups")
    b.bars("Biggest groups by size", [q('topk(20, atlas_catalog_group_bytes{group!="(other)"})', "{{group}}", instant=True)], "bytes")
    b.bars("Biggest groups by releases", [q('topk(20, atlas_catalog_group_releases{group!="(other)"})', "{{group}}", instant=True)])
    b.table("Top 100 groups", [q("atlas_catalog_group_bytes", "A"), q("atlas_catalog_group_releases", "B")],
            {"group": ("Group", None), "Value #A": ("Size", "bytes"), "Value #B": ("Releases", "short")}, w=12, sort="Size",
            links=[("This group in the pipeline", '/d/atlas-pipeline?var-group=${__data.fields["Group"]}&${__url_time_range}')])
    b.graph("Busiest groups now, articles per second", [q("topk(15, sum by (group) (rate(atlas_articles_total[5m])))", "{{group}}")], h=10)
    b.row("Video release groups")
    b.stat("Video releases", "sum(atlas_catalog_video_releases)", w=6, description="Stored in Movies or TV, with a real name.")
    b.stat("With a release group", 'sum(atlas_catalog_video_releases{release_group="found"}) / sum(atlas_catalog_video_releases)', "percentunit", w=6)
    b.stat("Release groups seen", 'count(atlas_catalog_release_group_releases{release_group!="(other)"})', w=6, description="Shown: the top 150, the rest summed as (other).")
    b.stat("Biggest release group", 'topk(1, atlas_catalog_release_group_releases{release_group!="(other)"})', w=6)
    b.bars("Top 25 release groups, video releases", [q('topk(25, atlas_catalog_release_group_releases{release_group!="(other)"})', "{{release_group}}", instant=True)], w=12, h=14)
    b.table("Top 150 release groups", [q('atlas_catalog_release_group_releases', "A"), q('atlas_catalog_release_group_releases / scalar(atlas_catalog_video_releases{release_group="found"})', "B")],
            {"release_group": ("Release group", None), "Value #A": ("Video releases", "short"), "Value #B": ("Share", "percentunit")}, h=14, sort="Video releases")
    b.row("PreDB")
    b.stat("Match rate, all releases", 'atlas_catalog_releases{state="predb_matched"} / ignoring(state) atlas_catalog_releases{state="total"}', "percentunit", w=4)
    b.stat("Match rate, video", 'sum(atlas_catalog_predb_root_releases{root=~"Movies|TV",matched="yes"}) / sum(atlas_catalog_predb_root_releases{root=~"Movies|TV"})', "percentunit", w=4)
    b.stat("Releases matched", 'atlas_catalog_releases{state="predb_matched"}', w=4)
    b.stat("Titles in the PreDB", "atlas_predb_titles", w=4)
    b.stat("Matched in range", "sum(increase(atlas_predb_matches_total[$__range]))", w=4)
    b.stat("Title backlog", "atlas_predb_late_behind", w=4, description="Titles the late matcher has yet to compare with old releases.")
    b.pie("Prerelease map: matches by PreDB category", "atlas_catalog_predb_category_releases", "{{category}}")
    b.pie("Matches by feed", "atlas_catalog_predb_feed_releases", "{{feed}}")
    b.pie("Matches by how they matched", "atlas_catalog_predb_kind_releases", "{{kind}}")
    b.table("Match rate per category", [q('sum by (root) (atlas_catalog_predb_root_releases{matched="yes"})', "A"), q('sum by (root) (atlas_catalog_predb_root_releases)', "B"), q('sum by (root) (atlas_catalog_predb_root_releases{matched="yes"}) / sum by (root) (atlas_catalog_predb_root_releases)', "C")],
            {"root": ("Category", None), "Value #A": ("Matched", "short"), "Value #B": ("Releases", "short"), "Value #C": ("Rate", "percentunit")}, sort="Matched")
    b.graph("Matches per hour, by category", [q("sum by (category) (rate(atlas_predb_matches_total[15m])) * 3600", "{{category}}")], stacked=True)
    return b


def float_label(n):
    """A bound as the `le` label Rust writes it."""
    return str(n)


def nntp():
    server = variable("server", "Server", "label_values(atlas_nntp_connections, server)")
    command = variable("command", "Command", "label_values(atlas_nntp_commands_total, command)")
    sel = 'server=~"$server"'
    b = Board("atlas-nntp", "Atlas: NNTP servers", "Connections, bandwidth, latency and failures, in total and per server.", [server, command])
    b.row("In aggregate")
    b.stat("Connections open", 'sum(atlas_nntp_connections{state="open"})', w=4)
    b.stat("In use", 'sum(atlas_nntp_connections{state="in_use"})', w=4)
    b.stat("Limit", 'sum(atlas_nntp_connections{state="limit"})', w=4, description="What Atlas allows itself; below Configured when a provider refused more.")
    b.stat("Configured", 'sum(atlas_nntp_connections{state="configured"})', w=4)
    b.stat("Requests waiting", "sum(atlas_nntp_waiting_requests)", w=4)
    b.stat("Failures in range", f"sum(increase(atlas_nntp_failures_total{{{REAL_FAILURES}}}[$__range]))", w=4, thresholds=[{"color": "green", "value": None}, {"color": "orange", "value": 1}, {"color": "red", "value": 100}])
    b.graph("Connections, total", [q(f'sum by (state) (atlas_nntp_connections{{state=~"open|in_use|limit"}})', "{{state}}")])
    b.graph("Bandwidth, total", [q('sum by (kind) (rate(atlas_nntp_traffic_bytes_total[5m]))', "{{kind}}")], "Bps", description="wire is off the socket, text is after decompression.")
    b.row("Per server")
    b.table("Servers now", [
        q('atlas_nntp_connections{state="open"}', "A"), q('atlas_nntp_connections{state="in_use"}', "B"), q('atlas_nntp_connections{state="limit"}', "C"),
        q('atlas_nntp_connections{state="configured"}', "D"), q("atlas_nntp_waiting_requests", "E"), q('rate(atlas_nntp_traffic_bytes_total{kind="wire"}[5m])', "F"),
        q('increase(atlas_nntp_traffic_bytes_total{kind="wire"}[$__range])', "G"), q("increase(atlas_nntp_headers_total[$__range])", "H"),
        q(f"sum by (server) (increase(atlas_nntp_failures_total{{{REAL_FAILURES}}}[$__range]))", "I"),
    ], {"server": ("Server", None), "Value #A": ("Open", "short"), "Value #B": ("In use", "short"), "Value #C": ("Limit", "short"), "Value #D": ("Configured", "short"),
        "Value #E": ("Waiting", "short"), "Value #F": ("Now", "Bps"), "Value #G": ("Bytes in range", "bytes"), "Value #H": ("Headers", "short"), "Value #I": ("Failures", "short")}, w=24, h=8, sort="Bytes in range")
    b.graph("Connections per server", [q(f'atlas_nntp_connections{{{sel},state="open"}}', "{{server}}")], stacked=True)
    b.graph("Connections in use per server", [q(f'atlas_nntp_connections{{{sel},state="in_use"}}', "{{server}}")], stacked=True)
    b.graph("Bandwidth per server", [q(f'rate(atlas_nntp_traffic_bytes_total{{{sel},kind="wire"}}[5m])', "{{server}}")], "Bps", stacked=True)
    b.graph("Requests waiting per server", [q(f"atlas_nntp_waiting_requests{{{sel}}}", "{{server}}")])
    b.graph("Server state", [q(f'atlas_nntp_server_state{{{sel}}} == 1', "{{server}}: {{state}}")], description="Which state each server is in: ok, resting, login rejected or article only.")
    b.graph("Connection attempts per minute", [q(f"sum by (server, result) (rate(atlas_nntp_connects_total{{{sel}}}[5m])) * 60", "{{server}} {{result}}")])
    b.row("Failures")
    b.graph("When failures hit, per server", [q(f"sum by (server) (increase(atlas_nntp_failures_total{{{sel},{REAL_FAILURES}}}[$__rate_interval]))", "{{server}}")], bars=True, w=12, description="Each bar is the failures in that slice of time.")
    b.graph("When failures hit, per kind", [q(f"sum by (kind) (increase(atlas_nntp_failures_total{{{sel},{REAL_FAILURES}}}[$__rate_interval]))", "{{kind}}")], bars=True, w=12)
    b.table("Failures in range, server and kind", [q(f"increase(atlas_nntp_failures_total{{{sel}}}[$__range])", "A"), q(f"atlas_nntp_failures_total{{{sel}}}", "B"), q(f"atlas_nntp_last_failure_timestamp_seconds{{{sel}}} * 1000", "C")],
            {"server": ("Server", None), "kind": ("Kind", None), "Value #A": ("In range", "short"), "Value #B": ("Since restart", "short"), "Value #C": ("Last hit", "dateTimeAsIso")}, w=24, h=10, sort="In range",
            description="no_such_article and no_such_group are ordinary answers; the panels above leave them out.")
    b.row("Commands")
    csel = f'{sel},command=~"$command"'
    b.graph("Commands per second", [q(f"sum by (command) (rate(atlas_nntp_commands_total{{{csel}}}[5m]))", "{{command}}")], stacked=True)
    b.graph("Command results per second", [q(f"sum by (command, result) (rate(atlas_nntp_commands_total{{{csel}}}[5m]))", "{{command}} {{result}}")])
    b.graph("95th percentile per command", [q(f"histogram_quantile(0.95, sum by (command, le) (rate(atlas_nntp_command_duration_seconds_bucket{{{csel}}}[5m])))", "{{command}}")], "s")
    b.graph("95th percentile per server", [q(f"histogram_quantile(0.95, sum by (server, le) (rate(atlas_nntp_command_duration_seconds_bucket{{{csel}}}[5m])))", "{{server}}")], "s")
    b.graph("Received per command", [q(f'sum by (command) (rate(atlas_nntp_bytes_total{{{csel},direction="received"}}[5m]))', "{{command}}")], "Bps", stacked=True)
    b.graph("Sent per command", [q(f'sum by (command) (rate(atlas_nntp_bytes_total{{{csel},direction="sent"}}[5m]))', "{{command}}")], "Bps", stacked=True)
    return b


def functions():
    function = variable("function", "Function", "label_values(atlas_function_duration_seconds_count, function)")
    sel = 'function=~"$function"'
    b = Board("atlas-functions", "Atlas: Functions", "Every timed operation: what takes longest, what runs most, what moves the most bytes, what fails.", [function])
    b.row("What takes the longest")
    b.bars("Most time in range", [q(f'topk(25, sum by (function) (increase(atlas_function_duration_seconds_sum{{{sel}}}[$__range])))', "{{function}}", instant=True)], "s", h=14,
           links=[("Just this function", "/d/atlas-functions?var-function=${__field.labels.function}&${__url_time_range}")])
    b.bars("Longest on average", [q(f'topk(25, sum by (function) (increase(atlas_function_duration_seconds_sum{{{sel}}}[$__range])) / sum by (function) (increase(atlas_function_duration_seconds_count{{{sel}}}[$__range])))', "{{function}}", instant=True)], "s", h=14,
           links=[("Just this function", "/d/atlas-functions?var-function=${__field.labels.function}&${__url_time_range}")])
    b.table("Every function in range", [
        q(f"sum by (function) (increase(atlas_function_duration_seconds_count{{{sel}}}[$__range]))", "A"),
        q(f"sum by (function) (increase(atlas_function_duration_seconds_sum{{{sel}}}[$__range]))", "B"),
        q(f"sum by (function) (increase(atlas_function_duration_seconds_sum{{{sel}}}[$__range])) / sum by (function) (increase(atlas_function_duration_seconds_count{{{sel}}}[$__range]))", "C"),
        q(f"histogram_quantile(0.95, sum by (function, le) (increase(atlas_function_duration_seconds_bucket{{{sel}}}[$__range])))", "D"),
        q(f"sum by (function) (increase(atlas_function_errors_total{{{sel}}}[$__range]))", "E"),
        q(f"sum by (function) (increase(atlas_function_bytes_total{{{sel}}}[$__range]))", "F"),
        q(f"sum by (function) (atlas_function_in_progress{{{sel}}})", "G"),
    ], {"function": ("Function", None), "Value #A": ("Calls", "short"), "Value #B": ("Total time", "s"), "Value #C": ("Average", "s"), "Value #D": ("p95", "s"),
        "Value #E": ("Errors", "short"), "Value #F": ("Bytes", "bytes"), "Value #G": ("Running now", "short")}, w=24, h=14, sort="Total time",
        links=[("Just this function", '/d/atlas-functions?var-function=${__data.fields["Function"]}&${__url_time_range}')])
    b.row("Over time")
    b.graph("Time per second", [q(f"topk(15, sum by (function) (rate(atlas_function_duration_seconds_sum{{{sel}}}[5m])))", "{{function}}")], "s", stacked=True, description="Seconds spent per second, adding up overlapping calls.")
    b.graph("Calls per second", [q(f"topk(15, sum by (function) (rate(atlas_function_duration_seconds_count{{{sel}}}[5m])))", "{{function}}")])
    b.graph("95th percentile", [q(f"topk(15, histogram_quantile(0.95, sum by (function, le) (rate(atlas_function_duration_seconds_bucket{{{sel}}}[5m]))))", "{{function}}")], "s")
    b.graph("Running now", [q(f"topk(15, sum by (function) (atlas_function_in_progress{{{sel}}}))", "{{function}}")])
    b.graph("Errors per second", [q(f"sum by (function) (rate(atlas_function_errors_total{{{sel}}}[5m]))", "{{function}}")])
    b.heatmap("Heat map: call lengths", f'sum by (le) (increase(atlas_function_duration_seconds_bucket{{{sel}}}[$__rate_interval]))', unit="s")
    b.row("What takes the most bandwidth")
    b.bars("Most bytes in range", [q(f"topk(20, sum by (function) (increase(atlas_function_bytes_total{{{sel}}}[$__range])))", "{{function}}", instant=True)], "bytes")
    b.graph("Bandwidth per function", [q(f"topk(10, sum by (function, direction) (rate(atlas_function_bytes_total{{{sel}}}[5m])))", "{{function}} {{direction}}")], "Bps", h=10)
    b.row("The indexing machinery")
    b.graph("Share of time, by what it was doing", [q("rate(atlas_load_nanoseconds_total[5m]) / 1e9", "{{kind}}")], "s", description="writer_* are the database writers; xover_wire and connection_wait add up over all connections.")
    b.graph("Events per second", [q("rate(atlas_load_events_total[5m])", "{{kind}}")])
    return b


def pipeline():
    group = variable("group", "Group", "label_values(atlas_articles_total, group)")
    step = variable("step", "Step", "label_values(atlas_step_runs_total, step)")
    b = Board("atlas-pipeline", "Atlas: Pipeline", "From headers to named, categorized releases: indexing, the processing stage, the PreDB and ID providers.", [group, step])
    g = 'group=~"$group"'
    b.row("Indexing")
    b.stat("Workers", "atlas_indexer_workers", w=4)
    b.stat("Groups indexed", 'atlas_groups{state="indexed"}', w=4)
    b.stat("Groups blocked", 'atlas_groups{state="blocked"}', w=4)
    b.stat("Idle", "atlas_indexer_idle", w=4, description="1 when every group is caught up.")
    b.stat("Headers not saved yet", 'atlas_unsaved_headers{state="now"}', w=4)
    b.stat("Slices waiting for a writer", "atlas_writer_queued_slices", w=4)
    b.graph("Articles per second", [q(f"sum(rate(atlas_articles_total{{{g}}}[5m]))", "articles"), q(f"sum(rate(atlas_articles_skipped_total{{{g}}}[5m]))", "skipped")])
    b.graph("Post bytes indexed per second", [q(f"sum(rate(atlas_article_bytes_total{{{g}}}[5m]))", "post bytes")], "Bps")
    b.graph("Busiest groups", [q(f"topk(15, sum by (group) (rate(atlas_articles_total{{{g}}}[5m])))", "{{group}}")], stacked=True)
    b.table("Groups in range", [q(f"sum by (group) (increase(atlas_articles_total{{{g}}}[$__range]))", "A"), q(f"sum by (group) (increase(atlas_article_bytes_total{{{g}}}[$__range]))", "B"), q(f"atlas_group_last_indexed_timestamp_seconds{{{g}}} * 1000", "C")],
            {"group": ("Group", None), "Value #A": ("Articles", "short"), "Value #B": ("Post bytes", "bytes"), "Value #C": ("Last indexed", "dateTimeAsIso")}, h=8, sort="Articles")
    b.graph("Backfill chunks", [q('atlas_backfill_chunks{state=~"done|total"}', "{{state}}")])
    b.graph("Backfill chunks done per hour", [q('atlas_backfill_chunks{state="done_last_hour"}', "done in the last hour")])
    b.row("Processing stage")
    b.stat("Due now", 'atlas_stage_queue{state="due"}', w=6)
    b.stat("Waiting on bodies", 'atlas_stage_queue{state="net_wanted"}', w=6)
    b.stat("Failed", 'atlas_stage_queue{state="failed"}', w=6)
    b.stat("Backlog walk", 'atlas_stage_walk{state="done"} / ignoring(state) atlas_stage_walk{state="total"}', "percentunit", w=6)
    s = 'step=~"$step"'
    b.graph("Step runs per second, by step", [q(f"sum by (step) (rate(atlas_step_runs_total{{{s}}}[5m]))", "{{step}}")], stacked=True)
    b.graph("Step results per second", [q(f"sum by (result) (rate(atlas_step_runs_total{{{s}}}[5m]))", "{{result}}")], stacked=True)
    b.table("Steps in range", [
        q(f'sum by (step) (increase(atlas_step_runs_total{{{s}}}[$__range]))', "A"),
        q(f'sum by (step) (increase(atlas_step_runs_total{{{s},result="matched"}}[$__range]))', "B"),
        q(f'sum by (step) (increase(atlas_step_runs_total{{{s},result=~"retry|permanent|transient"}}[$__range]))', "C"),
    ], {"step": ("Step", None), "Value #A": ("Runs", "short"), "Value #B": ("Matched", "short"), "Value #C": ("Errors", "short")}, h=8, sort="Runs")
    b.graph("Rejected per second, by reason", [q("rate(atlas_stage_rejected_total[5m])", "{{reason}}")], stacked=True, h=8)
    b.graph("Article bodies per minute", [q("rate(atlas_stage_bodies_total[5m]) * 60", "bodies")])
    b.graph("Body bytes per second", [q("rate(atlas_stage_body_bytes_total[5m])", "bytes")], "Bps")
    b.row("PreDB feeds")
    b.table("Feeds", [q("atlas_predb_feed_failures", "A"), q("atlas_predb_feed_last_ok_timestamp_seconds * 1000", "B"), q('atlas_predb_feed_rows{kind="inserted"}', "C"), q('atlas_predb_feed_rows{kind="updated"}', "D"), q('atlas_predb_feed_rows{kind="last"}', "E")],
            {"feed": ("Feed", None), "Value #A": ("Failures in a row", "short"), "Value #B": ("Last answered", "dateTimeAsIso"), "Value #C": ("Inserted", "short"), "Value #D": ("Updated", "short"), "Value #E": ("Last poll's rows", "short")}, w=24, sort="Inserted")
    b.graph("Polls per hour, by result", [q("sum by (result) (rate(atlas_predb_feed_polls_total[15m])) * 3600", "{{result}}")], stacked=True, w=8)
    b.graph("Titles in the PreDB", [q("atlas_predb_titles", "titles"), q("atlas_predb_late_behind", "not compared yet")], w=8)
    b.graph("Lookups per second", [q("sum by (result) (rate(atlas_predb_lookups_total[5m]))", "{{result}}")], stacked=True, w=8)
    b.row("ID providers")
    b.graph("Requests per minute", [q("sum by (provider, result) (rate(atlas_media_requests_total[5m])) * 60", "{{provider}} {{result}}")], stacked=True)
    b.graph("Titles in the cache", [q("atlas_media_titles", "titles")])
    return b


def system():
    b = Board("atlas-system", "Atlas: Database and process", "Scrape health, the processes, the database files and the Newznab API.")
    b.row("Processes")
    b.stat("Indexer scrape", 'up{job="atlas",role="indexer"}', w=3, thresholds=[{"color": "text", "value": None}], mappings=UP_MAP)
    b.stat("API scrape", 'up{job="atlas",role="api"}', w=3, thresholds=[{"color": "text", "value": None}], mappings=UP_MAP)
    b.stat("Indexer started", 'atlas_process_start_time_seconds{role="indexer"} * 1000', "dateTimeAsIso", w=6)
    b.stat("API started", 'atlas_process_start_time_seconds{role="api"} * 1000', "dateTimeAsIso", w=6)
    b.stat("Time to answer a scrape", "max(atlas_scrape_render_seconds)", "s", w=6)
    b.graph("Scrape up", [q('up{job="atlas"}', "{{role}}")], description="A gap is Atlas restarting or the Mac being away; the scrape picks up by itself.")
    b.graph("Restarts", [q("changes(atlas_process_start_time_seconds[1h])", "{{role}}")], description="Starts in the last hour, per process.")
    b.graph("CPU cores used", [q("rate(atlas_process_cpu_milliseconds_total[5m]) / 1000", "{{role}}")])
    b.graph("Memory", [q("atlas_process_resident_memory_bytes", "{{role}}")], "bytes")
    b.graph("Disk bytes per second", [q("rate(atlas_process_disk_bytes_total[5m])", "{{role}} {{direction}}")], "Bps")
    b.graph("Indexer errors", [q("increase(atlas_indexer_errors_total[5m])", "errors")], bars=True)
    b.row("Database")
    b.stat("Database files", 'sum(atlas_db_bytes{kind="db"})', "bytes", w=6)
    b.stat("Write-ahead logs", 'sum(atlas_db_bytes{kind="wal"})', "bytes", w=6)
    b.stat("Articles stored", 'atlas_db_rows{kind="articles"}', w=6)
    b.stat("Releases stored", 'atlas_db_rows{kind="releases"}', w=6)
    b.graph("File sizes", [q('atlas_db_bytes{kind="db"}', "{{file}}")], "bytes", stacked=True)
    b.graph("Write-ahead logs", [q('atlas_db_bytes{kind="wal"}', "{{file}}")], "bytes", stacked=True)
    b.graph("Catalog walk: how long it takes", [q("atlas_catalog_scan_duration_seconds", "last walk")], "s")
    b.graph("Catalog walk: rows", [q("atlas_catalog_scan_rows", "{{state}}")])
    b.row("Newznab API")
    b.stat("Requests in range", "sum(increase(atlas_api_requests_total[$__range]))", w=6)
    b.stat("5xx in range", 'sum(increase(atlas_api_requests_total{status=~"5.."}[$__range]))', w=6, thresholds=GOOD_BAD)
    b.stat("Answered in range", "sum(increase(atlas_api_response_bytes_total[$__range]))", "bytes", w=6)
    b.stat("Slowest average", 'max(rate(atlas_function_duration_seconds_sum{function=~"api.*"}[5m]) / rate(atlas_function_duration_seconds_count{function=~"api.*"}[5m]))', "s", w=6)
    b.graph("Requests per minute", [q("sum by (endpoint, status) (rate(atlas_api_requests_total[5m])) * 60", "{{endpoint}} {{status}}")], stacked=True)
    b.graph("95th percentile per endpoint", [q('histogram_quantile(0.95, sum by (function, le) (rate(atlas_function_duration_seconds_bucket{function=~"api.*|search.*|nzb.*"}[5m])))', "{{function}}")], "s")
    return b


BOARDS = [overview, content, nntp, functions, pipeline, system]


def build():
    out = HERE / "dashboards"
    out.mkdir(exist_ok=True)
    for make in BOARDS:
        board = make().build()
        (out / f"{board['uid']}.json").write_text(json.dumps(board, indent=1) + "\n")
        print(f"{board['uid']}: {len(board['panels'])} panels")


def call(url, method, path, body=None):
    token = base64.b64encode(f"{os.environ.get('GRAFANA_USER', 'admin')}:{os.environ.get('GRAFANA_PASSWORD', 'admin')}".encode())
    request = urllib.request.Request(
        url.rstrip("/") + path, method=method, data=json.dumps(body).encode() if body is not None else None,
        headers={"Authorization": "Basic " + token.decode(), "Content-Type": "application/json"},
    )
    with urllib.request.urlopen(request, timeout=120) as reply:
        return json.load(reply)


def push(url):
    try:
        call(url, "GET", f"/api/folders/{FOLDER['uid']}")
    except urllib.error.HTTPError as e:
        if e.code != 404:
            raise
        call(url, "POST", "/api/folders", FOLDER)
    for make in BOARDS:
        board = make().build()
        reply = call(url, "POST", "/api/dashboards/db",
                     {"dashboard": board, "folderUid": FOLDER["uid"], "overwrite": True, "message": "dashboards.py push"})
        print(reply["url"])


if __name__ == "__main__":
    if sys.argv[1:2] == ["build"]:
        build()
    elif sys.argv[1:2] == ["push"] and len(sys.argv) == 3:
        push(sys.argv[2])
    else:
        sys.exit(__doc__)
