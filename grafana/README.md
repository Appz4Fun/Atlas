# Grafana dashboards

Eight dashboards for Atlas's [Prometheus metrics](../README.md#metrics), exported for sharing. Six are also on the [dashboards page](https://appz4fun.github.io/Atlas/); the sealed log's and the backfill and realtime one have no screenshot yet.

`atlas-progress.json`, "Atlas: Backfill and realtime", answers two questions: how far the backfill has come (per group and overall: article numbers indexed against each group's first and last on its server, the oldest post and day chunk reached, the pace and an ETA), and whether live indexing keeps up (articles behind each group's last number, the age of its newest indexed post, the indexing and processing queues, net lane claims per shard, errors and panics). Its `Atlas` variable picks one scrape target, since the same process can be scraped at two addresses.

To import one, go to Dashboards, then New, then Import, upload its JSON and pick your Prometheus or VictoriaMetrics data source.

`deploy/grafana/dashboards.py` is the source of these dashboards and can push them all at once. After changing it, run `python3 deploy/grafana/dashboards.py export` so these match.

## Screenshots

The screenshots cover 50 minutes of a real instance, 03:25 to 04:15 UTC on 2 November 2026. Its address, host name and news server names have been replaced with placeholders, so `server-a` to `server-h` stand for eight servers and aren't their real names.

## The page

GitHub Pages serves this folder from the `gh-pages` branch. After changing anything here and merging it, publish it from an up-to-date `main`:

```bash
git push --force origin "$(git subtree split --prefix grafana main):refs/heads/gh-pages"
```
