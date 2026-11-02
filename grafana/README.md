# Grafana dashboards

Six dashboards for Atlas's [Prometheus metrics](../README.md#metrics), exported from Grafana for sharing. They're also on the [dashboards page](https://appz4fun.github.io/Atlas/).

To import one, go to Dashboards, then New, then Import, upload its JSON and pick your Prometheus or VictoriaMetrics data source.

`deploy/grafana/dashboards.py` is the source of these dashboards and can push them all at once. After changing it, export these again so they match.

## Screenshots

The screenshots cover 50 minutes of a real instance, 03:25 to 04:15 UTC on 2 November 2026. Its address, host name and news server names have been replaced with placeholders, so `server-a` to `server-h` stand for eight servers and aren't their real names.

## The page

GitHub Pages serves this folder from the `gh-pages` branch. After changing anything here and merging it, publish it from an up-to-date `main`:

```bash
git push --force origin "$(git subtree split --prefix grafana main):refs/heads/gh-pages"
```
