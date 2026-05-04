# News Lab SearXNG

Local SearXNG metasearch deployment for News Lab.

## Start

```bash
cd deploy/searxng
docker compose up -d
```

The instance listens on localhost only:

```text
http://127.0.0.1:8888
```

## Test

```bash
curl 'http://127.0.0.1:8888/search?q=LLM%20inference&format=json'
```

## Configure News Lab

Add this to `.env`:

```env
SEARXNG_URL=http://127.0.0.1:8888
```

News Lab uses this endpoint for:

- SearXNG supplemental news results
- enterprise case search in project radar details

## Stop

```bash
cd deploy/searxng
docker compose down
```
