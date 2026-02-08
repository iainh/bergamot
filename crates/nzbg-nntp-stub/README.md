# nzbg-nntp-stub

Minimal NNTP stub server for integration tests. It serves deterministic message
bodies from a JSON fixture file and can inject delays or disconnects.

## Run

```sh
cargo run -p nzbg-nntp-stub -- \
  --bind 127.0.0.1:3119 \
  --fixtures fixtures/nntp/fixtures.json
```

## Fixture format

```json
{
  "greeting": "200 nzbg test server ready",
  "groups": {
    "alt.test": {
      "article_ids": ["1@test"],
      "missing_articles": ["missing@test"],
      "articles": {
        "1@test": "line1\nline2"
      }
    }
  }
}
```

## Failure injection

- `--disconnect-after N` disconnects after N commands.
- `--delay-ms MS` sleeps for the given milliseconds before responding.
- `--require-auth` forces AUTHINFO USER/PASS before GROUP/STAT/BODY.

## Examples

### Require authentication

```sh
cargo run -p nzbg-nntp-stub -- --require-auth --username nzbg --password test
```

### Delay and disconnect

```sh
cargo run -p nzbg-nntp-stub -- --delay-ms 250 --disconnect-after 5
```
