#!/usr/bin/env python3
"""Atlas's Grafana dashboards, as code.

    dashboards.py build             write dashboards/*.json
    dashboards.py export            write the shareable copies in grafana/*.json at the top of the repo,
                                    which ask for a data source on import
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

    def graph(self, title, targets, unit="short", w=12, h=8, description="", stacked=False, links=(), bars=False,
              colors=None, palette=False):
        """`colors`: series name -> colour, for series that keep one; `palette`: the rest take the categorical order"""
        field = {"custom": {"fillOpacity": 25 if stacked or bars else 8, "lineWidth": 1, "showPoints": "never",
                            "spanNulls": False, "drawStyle": "bars" if bars else "line",
                            "stacking": {"mode": "normal" if stacked else "none", "group": "A"}}}
        self.panel(
            "timeseries", title, targets, w, h, description, unit,
            {"legend": {"displayMode": "table", "placement": "bottom", "calcs": ["lastNotNull", "max"], "showLegend": True,
                        "sortBy": "Max", "sortDesc": True},
             "tooltip": {"mode": "multi", "sort": "desc"}},
            dict(field, color={"mode": "palette-classic"}) if palette else field, links=links,
            overrides=[{"matcher": {"id": "byName", "options": name},
                        "properties": [{"id": "color", "value": {"mode": "fixed", "fixedColor": color}}]}
                       for name, color in (colors or {}).items()],
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

    def table(self, title, targets, columns, w=12, h=10, description="", sort=None, links=(), cells=None, ascending=False):
        """`columns`: label or `Value #A` -> (shown name, unit or None); `cells`: shown name -> extra field properties"""
        overrides = []
        renames = {}
        for source, (shown, unit) in columns.items():
            renames[source] = shown
            props = []
            if unit:
                props.append({"id": "unit", "value": unit})
            if links and source == next(iter(columns)):
                props.append({"id": "links", "value": [{"title": t, "url": u} for t, u in links]})
            props.extend((cells or {}).get(shown, []))
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
             "sortBy": [{"displayName": sort, "desc": not ascending}] if sort else []},
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
    b.table("Failures in range, server and kind", [q(f"increase(atlas_nntp_failures_total{{{sel}}}[$__range]) > 0", "A"), q(f"atlas_nntp_failures_total{{{sel}}} > 0", "B"), q(f"atlas_nntp_last_failure_timestamp_seconds{{{sel}}} * 1000", "C")],
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
    b.graph("Errors per second", [q(f"sum by (function) (rate(atlas_function_errors_total{{{sel}}}[5m])) > 0", "{{function}}")])
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
    b.graph("Scrape up", [q('up{job="atlas"}', "{{role}}")], description="A gap is Atlas restarting or the machine being away; the scrape picks up by itself.")
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


def sealed_log():
    b = Board("atlas-sealed-log", "Atlas: Sealed log",
              "The sealed log service and the offload feeding it: throughput, latency on both ends of the wire, and the disk. "
              "Service numbers come from job sealed-log, Atlas's from job atlas.")
    svc = 'job="sealed-log"'
    client = 'function=~"sealed_log.(fetch|verify|append)"'

    def mean(sum_, count):
        return f"sum(rate({sum_}[5m])) / sum(rate({count}[5m]))"

    b.row("At a glance")
    b.stat("Service", f"up{{{svc}}}", w=2, thresholds=[{"color": "text", "value": None}], mappings=UP_MAP)
    b.stat("Log size", f"sealed_log_bytes{{{svc}}}", "bytes", w=3)
    b.stat("Disk free", f'sealed_log_filesystem_bytes{{{svc},kind="avail"}}', "bytes", w=3)
    b.stat("Disk used", f'1 - sealed_log_filesystem_bytes{{{svc},kind="avail"}} / ignoring(kind) sealed_log_filesystem_bytes{{{svc},kind="size"}}',
           "percentunit", w=3, thresholds=[{"color": "green", "value": None}, {"color": "orange", "value": 0.8}, {"color": "red", "value": 0.95}])
    b.stat("Writing now", f"rate(sealed_log_bytes_appended_total{{{svc}}}[5m])", "Bps", w=3)
    b.stat("Reading now", f"rate(sealed_log_bytes_read_total{{{svc}}}[5m])", "Bps", w=3)
    b.stat("Freed from shards in range", 'sum(increase(atlas_sealed_log_offload_bytes_total{kind="freed"}[$__range]))', "bytes", w=4,
           description="Blob bytes the swaps took out of the shards. SQLite gives the space back only after a VACUUM.")
    b.stat("Errors in range",
           f"(sum(increase(sealed_log_request_errors_total{{{svc}}}[$__range])) or vector(0))"
           f" + (sum(increase(sealed_log_commit_failures_total{{{svc}}}[$__range])) or vector(0))"
           " + (sum(increase(atlas_sealed_log_offload_failures_total[$__range])) or vector(0))", w=3, thresholds=GOOD_BAD,
           description="Failed requests and commits in the service, and failed offload batches in Atlas.")

    b.row("Throughput")
    b.graph("Bytes per second in the service", [
        q(f"rate(sealed_log_bytes_appended_total{{{svc}}}[5m])", "written"),
        q(f"rate(sealed_log_bytes_read_total{{{svc}}}[5m])", "read"),
    ], "Bps", w=8, description="Value bytes, without the 20-byte record headers.")
    b.graph("Network bytes per second", [
        q(f"rate(sealed_log_network_bytes_total{{{svc}}}[5m])", "service {{direction}}"),
        q(f'sum by (function, direction) (rate(atlas_function_bytes_total{{{client}}}[5m]))', "atlas {{function}} {{direction}}"),
    ], "Bps", w=8, description="What crosses the wire. Verify reads a whole batch back, so offloading costs twice its bytes on the network.")
    b.graph("Offload per second", [
        q('sum by (kind) (rate(atlas_sealed_log_offload_bytes_total[5m]))', "{{kind}}"),
    ], "Bps", w=8, description="Sent: blob bytes appended. Freed: what left the shards (sent less changed blobs and the 21-byte places).")
    b.graph("Values per second", [
        q(f"rate(sealed_log_values_appended_total{{{svc}}}[5m])", "appended"),
        q(f"rate(sealed_log_values_read_total{{{svc}}}[5m])", "read"),
    ], w=8)
    b.graph("Requests per second", [q(f"sum by (op) (rate(sealed_log_requests_total{{{svc}}}[5m]))", "{{op}}")], w=8)
    b.graph("Group commits", [
        q(f"rate(sealed_log_commits_total{{{svc}}}[5m])", "commits per second"),
        q(f"rate(sealed_log_commit_appends_total{{{svc}}}[5m]) / rate(sealed_log_commits_total{{{svc}}}[5m])", "appends per commit"),
    ], w=8, description="Appends that arrive together share one fdatasync. More per commit means the disk is the limit.")

    b.row("Latency")
    for op in ("append", "read"):
        b.graph(f"{op.title()}: time in the service", [
            q(f'histogram_quantile({p}, sum by (le) (rate(sealed_log_request_duration_seconds_bucket{{{svc},op="{op}"}}[5m])))', f"p{int(p * 100)}")
            for p in (0.5, 0.95, 0.99)
        ], "s", w=8)
    b.graph("Time Atlas waits, per call", [
        q(f'histogram_quantile(0.95, sum by (function, le) (rate(atlas_function_duration_seconds_bucket{{{client}}}[5m])))', "{{function}} p95"),
        q(f'histogram_quantile(0.5, sum by (function, le) (rate(atlas_function_duration_seconds_bucket{{{client}}}[5m])))', "{{function}} p50"),
    ], "s", w=8, description="Connecting, the network and the service together. fetch is a reader's one blob; append and verify are offload batches.")
    def overhead(functions, op):
        waits = mean(f'atlas_function_duration_seconds_sum{{function=~"{functions}"}}',
                     f'atlas_function_duration_seconds_count{{function=~"{functions}"}}')
        spends = mean(f'sealed_log_request_duration_seconds_sum{{{svc},op="{op}"}}',
                      f'sealed_log_request_duration_seconds_count{{{svc},op="{op}"}}')
        return q(f"({waits}) - ({spends})", op)

    b.graph("Network overhead per call (mean)", [overhead("sealed_log.append", "append"), overhead("sealed_log.(fetch|verify)", "read")],
            "s", w=8, description="What Atlas waits less what the service spends: the wire, both TCP stacks, and connecting. "
                                  "The disk's own time is in the next panel.")
    b.graph("Disk: fdatasync and reads", [
        q(f'histogram_quantile({p}, sum by (le) (rate(sealed_log_fdatasync_duration_seconds_bucket{{{svc}}}[5m])))', f"fdatasync p{int(p * 100)}")
        for p in (0.5, 0.95, 0.99)
    ] + [
        q(f'histogram_quantile(0.95, sum by (le) (rate(sealed_log_disk_read_duration_seconds_bucket{{{svc}}}[5m])))', "read one value p95"),
    ], "s", w=8, description="The disk itself, with nothing of the network in it.")
    b.graph("Waiting for the writer", [
        q(f'histogram_quantile({p}, sum by (le) (rate(sealed_log_queue_duration_seconds_bucket{{{svc}}}[5m])))', f"p{int(p * 100)}")
        for p in (0.5, 0.95, 0.99)
    ], "s", w=8, description="An append queued behind the commit in progress. Growing waits mean writes come faster than the disk takes them.")

    b.row("Disk")
    b.graph("Log size", [q(f"sealed_log_bytes{{{svc}}}", "log")], "bytes", w=8)
    b.graph("Data disk", [
        q(f'sealed_log_filesystem_bytes{{{svc},kind="size"}} - ignoring(kind) sealed_log_filesystem_bytes{{{svc},kind="free"}}', "used"),
        q(f'sealed_log_filesystem_bytes{{{svc},kind="avail"}}', "available"),
    ], "bytes", stacked=True, w=8)
    b.stat("Segments", f"sealed_log_segments{{{svc}}}", w=4)
    b.stat("Days until the disk fills", f'sealed_log_filesystem_bytes{{{svc},kind="avail"}} / ignoring(kind) (deriv(sealed_log_bytes{{{svc}}}[6h]) * 86400) > 0',
           "d", w=4, description="At the rate the log grew over the last 6 hours.")

    b.row("Offload")
    b.graph("What the offload is doing", [q("max by (state) (atlas_sealed_log_offload_state)", "{{state}}")], stacked=True, w=8)
    b.graph("Files per minute", [q("sum by (result) (rate(atlas_sealed_log_offload_files_total[5m])) * 60", "{{result}}")], stacked=True, w=8,
            description="Changed: more articles were sealed into the blob while it was on its way, so it stays local and its copy goes unused.")
    b.graph("Share of time throttled", [q("rate(atlas_sealed_log_offload_throttled_milliseconds_total[5m]) / 1000", "throttled")], "percentunit", w=8,
            description="Waiting to stay under offload_mb_per_second. Near 0 means the network or the service is the limit, not the setting.")
    b.graph("Sweep", [q('atlas_sealed_log_sweep{kind=~"shards|shards_left"}', "{{kind}}")], w=8)
    b.stat("Last sweep finished", 'atlas_sealed_log_sweep{kind="last_done_timestamp_seconds"} * 1000', "dateTimeFromNow", w=4)
    b.stat("Last sweep moved", 'atlas_sealed_log_sweep{kind="last_bytes"}', "bytes", w=4)
    b.graph("Failures", [
        q("sum by (kind) (increase(atlas_sealed_log_offload_failures_total[5m]))", "offload {{kind}}"),
        q(f"sum by (op) (increase(sealed_log_request_errors_total{{{svc}}}[5m]))", "service {{op}}"),
        q(f"increase(sealed_log_checksum_failures_total{{{svc}}}[5m])", "checksum"),
        q('sum by (result) (increase(atlas_sealed_log_connects_total{result="failed"}[5m]))', "connect failed"),
    ], bars=True, w=12)
    b.graph("Connections", [
        q(f"sealed_log_connections{{{svc}}}", "open at the service"),
        q('sum by (role) (increase(atlas_sealed_log_connects_total[5m]))', "new from {{role}}"),
    ], w=12)
    return b

# the reference palette's steps for a dark surface: categorical slots in their fixed order, and the status colours
SERIES = ["#3987e5", "#d95926", "#199e70", "#c98500", "#d55181", "#008300", "#9085e9", "#e66767"]
GOOD, WARNING, SERIOUS, CRITICAL = "#0ca30c", "#fab219", "#ec835a", "#d03b3b"
MUTED = "#8e8d87"
LINK_PROGRESS = ("Backfill and realtime", "/d/atlas-progress?${__url_time_range}")
# a shard keeps its colour on every panel
SHARD_COLORS = {f"shard {i}": color for i, color in enumerate(SERIES)}


def one_host(board):
    """Every atlas_ series of `board` read from the one `$host`: the same process can be scraped at two addresses."""
    import re
    def pin(m):
        return f'{m.group(1)}{{host="$host",' if m.group(2) else f'{m.group(1)}{{host="$host"}}'
    for panel in board.panels:
        for t in panel.get("targets", []):
            t["expr"] = re.sub(r"\b(atlas_[a-z_]+)\b(\{)?", pin, t["expr"]).replace(',}', '}')
    return board


def progress():
    host = variable("host", "Atlas", 'label_values(atlas_build_info{role="indexer"}, host)', multi=False, include_all=False)
    group = variable("group", "Group", 'label_values(atlas_group_backfill_articles{host="$host"}, group)')
    b = Board("atlas-progress", "Atlas: Backfill and realtime",
              "How far the backfill has come, per group and overall, and whether live indexing and processing keep up. "
              "Article numbers count the gaps on the server too. Backfill goes newest month first, in day chunks for split groups.",
              [host, group])
    g = 'group=~"$group"'
    total = f'sum(atlas_group_backfill_articles{{state="total",{g}}})'
    covered = f'sum(atlas_group_backfill_articles{{state="covered",{g}}})'
    left = f"({total} - {covered})"
    # how fast what is left shrinks, over the last 6 hours: new posts add to it, so it is net of them
    shrink = f"(-deriv({left}[6h:5m]))"
    per_group_left = f'(atlas_group_backfill_articles{{state="total",{g}}} - ignoring(state) atlas_group_backfill_articles{{state="covered",{g}}})'
    behind = (f'clamp_min(atlas_group_article_number{{mark="last",{g}}} - ignoring(mark) '
              f'atlas_group_article_number{{mark="live_cursor",{g}}}, 0)')
    since_newest = f'time() - atlas_group_posted_timestamp_seconds{{end="newest",{g}}}'

    b.row("Backfill: how far along")
    b.stat("Done", f"{covered} / {total}", "percentunit", w=4,
           description="Article numbers indexed, out of every number between each group's first and last on its server.")
    b.stat("Articles left", left, w=4)
    b.stat("Shrinking by", f"{shrink} * 3600", w=4, description="Articles a hour, net of new posts, over the last 6 hours.")
    b.stat("ETA", f"{left} / ({shrink} > 0)", "s", w=4,
           description="Articles left at the last 6 hours' pace. No data means what is left is not shrinking.",
           mappings=[{"type": "special", "options": {"match": "null", "result": {"text": "not shrinking", "index": 0}}}])
    b.stat("Day chunks done", 'atlas_backfill_chunks{state="done"} / ignoring(state) atlas_backfill_chunks{state="total"}', "percentunit", w=4,
           description="Split groups backfill a day at a time; this is the share of those days done.")
    b.stat("Day chunks ETA", '(atlas_backfill_chunks{state="total"} - ignoring(state) atlas_backfill_chunks{state="done"}) '
                             '/ ignoring(state) (atlas_backfill_chunks{state="done_last_hour"} > 0) * 3600', "s", w=4,
           description="Days left at the last hour's pace.",
           mappings=[{"type": "special", "options": {"match": "null", "result": {"text": "none done this hour", "index": 0}}}])
    b.graph("Done", [q(f"{covered} / {total}", "done")], "percentunit", w=8, colors={"done": SERIES[0]})
    b.graph("Articles left", [q(left, "left")], w=8, colors={"left": SERIES[0]})
    b.graph("How far back, middle group", [
        q(f'time() - quantile(0.5, atlas_group_posted_timestamp_seconds{{end="oldest",{g}}})', "oldest post indexed"),
        q(f'time() - quantile(0.5, atlas_group_backfill_day_timestamp_seconds{{day="oldest_done",{g}}})', "oldest day chunk done"),
    ], "s", w=8, colors={"oldest post indexed": SERIES[0], "oldest day chunk done": SERIES[1]},
       description="How old the oldest indexed post is in the middle group. Climbing means backfill is reaching further back.")
    b.table("Per group", [
        q(f"atlas_group_backfill_articles{{state=\"covered\",{g}}} / ignoring(state) atlas_group_backfill_articles{{state=\"total\",{g}}}", "A"),
        q(per_group_left, "B"),
        q(f'atlas_group_article_number{{mark="first",{g}}}', "C"),
        q(f'atlas_group_article_number{{mark="backfill_cursor",{g}}}', "D"),
        q(f'atlas_group_article_number{{mark="live_cursor",{g}}}', "E"),
        q(f'atlas_group_article_number{{mark="last",{g}}}', "F"),
        q(f'atlas_group_posted_timestamp_seconds{{end="oldest",{g}}} * 1000', "G"),
        q(f'atlas_group_backfill_chunks{{state="done",{g}}} / ignoring(state) atlas_group_backfill_chunks{{state="total",{g}}}', "H"),
        q(f'atlas_group_backfill_day_timestamp_seconds{{day="oldest_done",{g}}} * 1000', "I"),
        q(f"{per_group_left} / (-deriv({per_group_left}[6h:5m]) > 0)", "J"),
    ], {"group": ("Group", None), "Value #A": ("Done", "percentunit"), "Value #B": ("Left", "short"),
        "Value #C": ("Server first", "none"), "Value #D": ("Backfill cursor", "none"), "Value #E": ("Live cursor", "none"),
        "Value #F": ("Server last", "none"), "Value #G": ("Oldest post", "dateTimeAsIso"),
        "Value #H": ("Days done", "percentunit"), "Value #I": ("Oldest day done", "dateTimeAsIso"), "Value #J": ("ETA", "s")},
        w=24, h=12, sort="Left",
        description="Article numbers from each group's furthest-along server. Split groups count their day chunks done as that share. ETA at the last 6 hours' pace, blank when not shrinking.",
        cells={"Done": [{"id": "custom.cellOptions", "value": {"type": "gauge", "mode": "basic"}},
                        {"id": "min", "value": 0}, {"id": "max", "value": 1},
                        {"id": "color", "value": {"mode": "fixed", "fixedColor": SERIES[0]}}]})

    b.row("Realtime: is it keeping up")
    lag_steps = [{"color": GOOD, "value": None}, {"color": WARNING, "value": 1800}, {"color": CRITICAL, "value": 7200}]
    b.stat("Articles behind", f"sum({behind})", w=4,
           description="New articles on the servers that the live cursors have not reached, as of each group's last pass.")
    b.stat("Groups behind", f"count({behind} > 0) or vector(0)", w=4)
    b.stat("Groups not moved in 30 min", f'count(changes(atlas_group_article_number{{mark="live_cursor",{g}}}[30m]) == 0) or vector(0)', w=4,
           thresholds=[{"color": GOOD, "value": None}, {"color": WARNING, "value": 1}, {"color": CRITICAL, "value": 20}],
           description="Groups whose live cursor has not moved in 30 minutes, from the database, so a restart does not hide them. "
                       "Quiet groups land here too: nothing new to index looks the same as not indexing.")
    b.stat("Newest post, middle group", f"quantile(0.5, {since_newest})", "s", w=4, thresholds=lag_steps,
           description="Age of the newest indexed post in the middle group: the lag behind Usenet in time.")
    b.stat("Caught up", "atlas_indexer_idle", w=4,
           mappings=[{"type": "value", "options": {"0": {"text": "working", "index": 0}, "1": {"text": "every group caught up", "index": 1}}}])
    b.stat("Indexing errors in range", "sum(increase(atlas_indexer_errors_total[$__range]))", w=4,
           thresholds=[{"color": GOOD, "value": None}, {"color": SERIOUS, "value": 1}])
    b.graph("Articles behind, the 8 furthest behind groups", [q(f"topk(8, {behind})", "{{group}}")], w=8, palette=True)
    b.graph("Newest post indexed, age, the 8 oldest", [q(f"topk(8, {since_newest})", "{{group}}")], "s", w=8, palette=True)
    b.graph("Indexed per second", [q(f"sum(rate(atlas_articles_total{{{g}}}[5m]))", "articles"),
                                   q(f"sum(rate(atlas_releases_seen_total{{{g}}}[5m]))", "releases seen")], w=8,
            colors={"articles": SERIES[0], "releases seen": SERIES[1]})
    b.table("Lag per group", [
        q(behind, "A"),
        q(f"{since_newest}", "B"),
        q(f"time() - atlas_group_last_indexed_timestamp_seconds{{{g}}}", "C"),
        q(f"sum by (group) (rate(atlas_articles_total{{{g}}}[15m]))", "D"),
    ], {"group": ("Group", None), "Value #A": ("Articles behind", "short"), "Value #B": ("Newest post age", "s"),
        "Value #C": ("Since last indexed", "s"), "Value #D": ("Articles a second", "short")},
        w=24, h=10, sort="Articles behind")

    b.row("Queues")
    b.graph("Headers fetched, not saved", [q('atlas_unsaved_headers{state="now"}', "waiting"), q('atlas_unsaved_headers{state="cap"}', "cap")], w=8,
            colors={"waiting": SERIES[0], "cap": MUTED}, description="Fetching pauses at the cap.")
    b.graph("Parse and save", [
        q("atlas_writer_queued_slices", "slices waiting for a writer"),
        q('atlas_function_in_progress{function="indexer.parse_slice"}', "parsing"),
        q('atlas_function_in_progress{function="indexer.save_slice"}', "saving"),
    ], w=8, colors={"slices waiting for a writer": SERIES[0], "parsing": SERIES[1], "saving": SERIES[2]})
    b.graph("Processing stage queue", [q('atlas_stage_queue{state="due"}', "due"), q('atlas_stage_queue{state="net_wanted"}', "waiting on bodies"),
                                       q('atlas_stage_queue{state="failed"}', "failed")], w=8,
            colors={"due": SERIES[0], "waiting on bodies": SERIES[1], "failed": SERIES[2]},
            description="Bounded counts: a big queue reads as its cap.")
    b.graph("Steps running now", [q('topk(8, atlas_function_in_progress{function=~"step[.].*"})', "{{function}}")], w=8, palette=True)
    b.graph("Stills", [q('atlas_function_in_progress{function="step.stills"}', "running"),
                       q('sum(rate(atlas_step_runs_total{step="stills"}[5m])) * 60', "runs a minute")], w=8,
            colors={"running": SERIES[0], "runs a minute": SERIES[1]})
    b.graph("Bodies", [q('atlas_function_in_progress{function="stage.fetch_body"}', "fetching now"),
                       q("rate(atlas_stage_bodies_total[5m]) * 60", "fetched a minute")], w=8,
            colors={"fetching now": SERIES[0], "fetched a minute": SERIES[1]})

    b.row("Net lane per shard")
    b.stat("Claims running", "sum(atlas_stage_net_claims) / (max(atlas_stage_net_claims_limit) * count(atlas_stage_net_claims))", "percentunit", w=4,
           description="Net claims running over every shard, out of net_claims_per_shard each.")
    b.stat("Claims per shard limit", "atlas_stage_net_claims_limit", w=4)
    b.graph("Claims per shard", [q("atlas_stage_net_claims", "shard {{shard}}"), q("atlas_stage_net_claims_limit", "limit")], w=16, h=8,
            colors={**SHARD_COLORS, "limit": MUTED})
    b.graph("Net visits a minute, per shard", [q('rate(atlas_stage_lane_total{lane="net_visits"}[5m]) * 60', "shard {{shard}}")], w=8, colors=SHARD_COLORS,
            description="Releases the net lane ran a step of.")
    b.graph("Finished by the net lane a minute, per shard", [q('rate(atlas_stage_lane_total{lane="net_finished"}[5m]) * 60', "shard {{shard}}")], w=8, colors=SHARD_COLORS,
            description="Releases the net lane left with no net step to run.")
    b.graph("Queue rows taken a minute, per shard", [q('rate(atlas_stage_lane_total{lane="queue_taken"}[5m]) * 60', "shard {{shard}}")], w=8, colors=SHARD_COLORS,
            description="The database lane.")

    b.row("Freezing")
    b.stat("Queued to freeze", 'sum(atlas_freeze_pending{state="queued"})', w=6,
           description="Releases the freezing rules queued to move out of the shards. Counted every 5 minutes.")
    b.stat("Ready to move", 'sum(atlas_freeze_pending{state="ready"})', w=6,
           description="Of those, the ones queued longer than wait_hours (12 by default), which the writers move now.")
    b.stat("Frozen a minute", 'sum(rate(atlas_frozen_events_total{event="frozen"}[10m])) * 60', w=6)
    b.stat("Longest queued", "time() - min(atlas_freeze_pending_oldest_timestamp_seconds)", "s", w=6,
           description="Moves start once this passes wait_hours. A visit to a release queues it afresh.")
    b.graph("Queued per shard", [q('atlas_freeze_pending{state="queued"}', "shard {{shard}}")], w=8, colors=SHARD_COLORS)
    b.graph("Ready per shard", [q('atlas_freeze_pending{state="ready"}', "shard {{shard}}")], w=8, colors=SHARD_COLORS)
    b.graph("Freezing a minute", [
        q('sum(rate(atlas_frozen_events_total{event="frozen"}[5m])) * 60', "frozen"),
        q('sum(rate(atlas_frozen_events_total{event="backlog_queued"}[5m])) * 60', "queued by the backlog walk"),
        q('sum(rate(atlas_frozen_events_total{event="busy"}[5m])) * 60', "ticks the lock was busy"),
    ], w=8, colors={"frozen": SERIES[0], "queued by the backlog walk": SERIES[1], "ticks the lock was busy": SERIES[2]})

    b.row("Errors")
    bad = [{"color": GOOD, "value": None}, {"color": CRITICAL, "value": 1}]
    b.stat("Step panics in range", "sum(increase(atlas_step_panics_total[$__range])) or vector(0)", w=6, thresholds=bad)
    b.stat("Step errors in range", 'sum(increase(atlas_step_runs_total{result=~"retry|permanent|transient"}[$__range])) or vector(0)', w=6,
           thresholds=[{"color": GOOD, "value": None}, {"color": SERIOUS, "value": 1}])
    b.stat("Function errors in range", "sum(increase(atlas_function_errors_total[$__range])) or vector(0)", w=6,
           thresholds=[{"color": GOOD, "value": None}, {"color": SERIOUS, "value": 1}])
    b.stat("Indexer errors in range", "sum(increase(atlas_indexer_errors_total[$__range])) or vector(0)", w=6,
           thresholds=[{"color": GOOD, "value": None}, {"color": SERIOUS, "value": 1}])
    b.graph("Step errors and panics, by step", [
        q('topk(8, sum by (step) (increase(atlas_step_runs_total{result=~"retry|permanent|transient"}[5m])))', "{{step}}"),
    ], bars=True, w=8, palette=True)
    b.graph("Step panics, by step", [q("sum by (step) (increase(atlas_step_panics_total[5m]))", "{{step}}")], bars=True, w=8, palette=True)
    b.graph("Function errors, the top 8", [q("topk(8, sum by (function) (increase(atlas_function_errors_total[5m])))", "{{function}}")], bars=True, w=8, palette=True)
    return one_host(b)


BOARDS = [overview, content, nntp, functions, pipeline, system, sealed_log, progress]


def build():
    out = HERE / "dashboards"
    out.mkdir(exist_ok=True)
    for make in BOARDS:
        board = make().build()
        (out / f"{board['uid']}.json").write_text(json.dumps(board, indent=1) + "\n")
        print(f"{board['uid']}: {len(board['panels'])} panels")


EXPORT_INPUTS = {
    "__inputs": [{"name": "DS_PROMETHEUS", "label": "Prometheus or VictoriaMetrics", "description": "", "type": "datasource",
                  "pluginId": "prometheus", "pluginName": "Prometheus"}],
    "__requires": [{"type": "datasource", "id": "prometheus", "name": "Prometheus", "version": "1.0.0"}],
}


def exported(board):
    """A board as Grafana's "Export for sharing externally" writes it: keys sorted, the data source an input."""
    text = json.dumps(board, sort_keys=True).replace(json.dumps(DATASOURCE["uid"]), '"${DS_PROMETHEUS}"')
    board = json.loads(text)
    board["id"] = None
    return json.dumps({**EXPORT_INPUTS, **json.loads(json.dumps(board, sort_keys=True))}, indent=2) + "\n"


def export():
    out = HERE.parent.parent / "grafana"
    for make in BOARDS:
        board = make().build()
        (out / f"{board['uid']}.json").write_text(exported(board))
        print(f"grafana/{board['uid']}.json")


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
    elif sys.argv[1:2] == ["export"]:
        export()
    elif sys.argv[1:2] == ["push"] and len(sys.argv) == 3:
        push(sys.argv[2])
    else:
        sys.exit(__doc__)
