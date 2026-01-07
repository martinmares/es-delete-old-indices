# es-delete-old-indices

Delete old Elasticsearch indices based on the date encoded in the index name.

The tool supports two name patterns:
- Monthly indices: `PREFIXYYYY-MM` or `PREFIXYYYY.MM`
- Weekly indices: `PREFIXYYYY-W` or `PREFIXYYYY-WW` (ISO week number, 1-53)

The age is always calculated in **months** and compared to the `--older-than` threshold.

## Usage

```bash
es-delete-old-indices \
  --url="http://localhost:9200" \
  --index-prefix="zis-audit-" \
  --older-than=37m
```

By default it runs in dry-run mode and only prints what would be deleted.
Add `--no-dryrun` to actually delete the indices.

## Options

- `--url` Elasticsearch base URL (required)
- `--index-prefix` index name prefix (default: `zis-audit-`)
- `--older-than` month threshold (default: `25m`)
- `--date-pattern` `month` (default) or `week`
- `--no-dryrun` perform deletions (dry-run otherwise)
- `--username` and `--password` basic auth (must be provided together)

## Help output (example)

The built-in `--help` includes usage, options, and examples:

```text
$ es-delete-old-indices --help
Delete old indices by name (monthly or weekly patterns)

Usage: es-retention [OPTIONS]

Options:
      --url <URL>
      --username <USERNAME>
      --password <PASSWORD>
      --index-prefix <INDEX_PREFIX>    [default: zis-audit-]
      --older-than <OLDER_THAN>        [default: 25m]
      --date-pattern <DATE_PATTERN>    [default: month]
      --no-dryrun
  -h, --help
  -V, --version

Examples:
  es-delete-old-indices --url http://localhost:9200 --index-prefix zis-audit- --older-than 25m
  es-delete-old-indices --url http://localhost:9200 --index-prefix kafka-zis-external-orders-notify- --date-pattern week --older-than 21m --no-dryrun
```

## Examples

Monthly indices (default pattern):

```bash
es-delete-old-indices \
  --url="http://celzisp403.server.cetin:9200" \
  --index-prefix="zis-audit-" \
  --older-than=37m \
  --no-dryrun
```

Weekly indices (e.g. `kafka-zis-external-orders-notify-2025-1`):

```bash
es-delete-old-indices \
  --url="http://celzisp403.server.cetin:9200" \
  --index-prefix="kafka-zis-external-orders-notify-" \
  --date-pattern=week \
  --older-than=21m \
  --no-dryrun
```

## Notes

- Only indices whose names match the selected pattern are considered.
- Age is computed in months using the first day of the month derived from the
  index date (for weekly indices this is the month of the ISO week start).
