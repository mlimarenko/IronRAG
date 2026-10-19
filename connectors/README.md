# IronRAG connectors

Source connectors that push content into IronRAG, and the adapter framework
they share. Four sources ship today — BookStack, Confluence, Git repositories
and YouTrack — plus `echo`, a reference implementation the framework's own
tests run against.

Connectors live in the IronRAG repository and release on the IronRAG version.

## What the framework does

You implement one [`SourceAdapter`](src/ironrag_connector/source.py) and the
framework handles the rest:

- the IronRAG HTTP client (`upload` / `replace` / `delete` / `find_by_external_key`)
- YAML routing: adapter-emitted facts → `workspace-slug/library-slug`
- per-`kind` policies (`on_new` / `on_changed` / `on_missing` / `on_duplicate_content` /
  `include_comments`), hot-reloaded from `routing.yaml` and rejected for kinds the
  adapter never emits
- persistent SQLite cursor and verified document-location state, so a restart
  ships only the diff and can reconcile historical routes
- the periodic sync loop, the two-phase orphan reaper (primary kinds against the
  census, dependent kinds against durable parent membership), and in-sweep
  de-duplication
- a FastAPI server: `/health`, `/sync/run`, and optional `/webhook/{name}` mounts
- a pidfile lock, structured per-item logging, and run modes (`poll` / `webhook` / `both`)

## Running one

Each source has a deployment template under [`deploy/`](deploy/):

```bash
cd connectors/deploy/bookstack
cp .env.example .env                  # IRONRAG_BASE_URL, IRONRAG_API_TOKEN, ADMIN_BEARER_TOKEN, vendor credentials
cp routing.yaml.example routing.yaml  # the workspace/library slugs this source writes into
IRONRAG_VERSION=v0.6.0 docker compose up -d
```

The images are `pipingspace/ironrag.{bookstack,confluence,gitrepos,youtrack}`,
tagged with the IronRAG release tag exactly as published — including the leading
`v`, so `v0.6.0`, not `0.6.0`. `IRONRAG_VERSION` is required rather than
defaulted: select the release explicitly.

`GET /health` answers with the source name and the build version, which is how
you check what is actually running.

## State volume permissions

The containers run as uid 10001. The state volume must be writable by that user.
To set ownership for the BookStack state volume:

```bash
docker run --rm -v ironrag-bookstack-state:/state alpine chown -R 10001:10001 /state
```

For the Git connector, a mounted deploy key must be owned by 10001 and
belongs at `/var/lib/ironrag-connector/.ssh/` — the connector user's home.

## Sources

| Source | Reads | Notes |
|---|---|---|
| `bookstack` | pages, books, shelves, attachments, images | webhook + poll; multimodal |
| `confluence` | pages and attachments | webhook + poll |
| `gitrepos` | files in git repositories | poll; shells out to the `git` CLI, so its image carries `git` and `openssh-client` |
| `youtrack` | Knowledge Base articles, issues, comments and safe related metadata | poll only; deployment requires a source/target reader-set proof |
| `echo` | an in-memory dict | the reference adapter; not shipped as an image |

A sweep may delete a previously indexed document only after a complete
census. An adapter that cannot enumerate part of its source (a git repo that
fails to fetch, a listing record it cannot parse) still pushes what it did
enumerate, then ends the sweep with `IncompleteCensusError`; the report shows
`census_complete: false` and nothing is reaped until a later sweep proves the
absence. Files an operator excludes by configuration (patterns, size ceiling,
repo or branch lists) are outside the census scope and follow `on_missing`.

Dependents (a page's attachments and inline images) are proven by their
parent, not by the census: a complete parent fetch claims its dependent set
durably and releases what it omitted. Released copies are deleted only by the
dependent reaper after a complete sweep proves every enumerated owning parent;
a legacy owner without claims can still reference a shared copy. Deleting a
page releases its membership, but its dependents await the same gate, each
still obeying its own kind's
`on_missing` (BookStack images default to `ignore` and survive). The report's
`dependent_census_complete` says whether every enumerated parent that owns
copies was proven; until then dependent kinds are not reaped. A dependent the
server proves is unsupported (`failureDisposition=content_unsupported`, from a
failed operation or a synchronous 400 admission problem) is
terminally accounted -- claimed, not materialized, kept against the reaper --
so it neither errors nor withholds the phase; any other dependent failure stays
an error and withholds it.

A validated, identity-bound terminal operation failure of a POLICY primary
does not prevent applying its actual fetched dependent set. Only a settled
dependent phase records that real census (including a genuinely empty set);
dependent failure or deferral records none. The primary still counts as an
error and webhooks still fail. Its cursor is not advanced, so a later sweep
retries the primary without changing historical failed operations. Unvalidated
failures, incomplete fetches and REQUIRED materializations remain fail-closed.

In `webhook` mode, connectors with dependents automatically request a background
canonical sweep after each authenticated, parsed webhook delivery, including a
handler failure. Requests coalesce; the webhook response does not wait for the
sweep. Cleanup uses the same complete-census gate and waits behind any active
manual sweep. A sweep never requests another sweep itself: an unproven owner
keeps its copies until a later delivery or manual sweep proves it. The periodic
poller remains disabled in this mode.

### YouTrack production admission

The YouTrack connector embeds fully paginated comments, attachment metadata and
eligible issue links in each article or issue document. It reads related
collections twice consecutively, orders comments by `(created, id)`, and puts
the normalized related state into the parent change token. Comment sync is
switched per kind in `routing.yaml` -- `policies.issue.include_comments` and
`policies.article.include_comments`, both default `true` and independent, and
hot-reloaded with the file. A disabled kind never requests its comment
endpoint, its documents carry no comment section, and its change tokens move so
the next sweep rematerializes it. There is no environment counterpart.
`YOUTRACK_CUSTOM_FIELDS` is the only custom-field allowlist; requesting all
project fields is not supported. Each admitted field can route by the exact
fact name `custom_field:<verbatim YouTrack name>`. There are no fixed field
aliases or case/slug normalization, and the adapter discards an unconfigured
field before inspecting or copying its value.

The token account needs `Read Project Basic`, `Read Article` and `Read Issue`
throughout the admitted scope, `Read Article Comment` / `Read Issue Comment`
for each kind whose comments are enabled, plus access to every included
attachment descriptor and linked issue target. Issue comment completeness is
checked against `Issue.commentsCount` while issue comments are enabled. The
article-comments endpoint promises only all comments accessible to that account
and articles expose no independent comment count, so article-comment
completeness also requires an explicit permission/audience admission.

Both YouTrack parent kinds declare their materialized projection and their
complete-census disappearance as `REQUIRED`. A visibility or ancestry state
that cannot prove the indexed projection safe, a related child that disappears,
or a missing parent therefore removes every exact-key copy even when routing
policy says `on_missing=ignore` for an explicit source-deletion decision. A
complete safe replacement instead accepts independent visibility denial before
payload admission and retires historical route copies without deleting their
foreign winners. Failed or deferred payloads retain that accepted fence and do
not advance the source token; unknown acceptance is not claimed as protection.

With a valid routing file, no matching rule and no default is an intentional
absence for these `REQUIRED` kinds: every exact-key copy is removed, including
under `on_missing=ignore`. Catalog resolution failures and rejected routing
reloads are not absence decisions; they preserve indexed content and the last
valid routing snapshot.

`UnlimitedVisibility` alone does not prove that an IronRAG reader may see the
same data: it is evaluated underneath YouTrack project permissions. Before
enabling a production route, record evidence that the target library's readers
are a subset of the readers for the projected YouTrack corpus, or provide an
IAM/ACL mapping. That containment must hold through every permitted ACL change,
because polling cannot make the final YouTrack read and the IronRAG mutation
one transaction. If an item can become narrower than the target audience, the
route is inadmissible without an ACL-aware query-time authorization boundary;
`REQUIRED` cleanup only bounds stale persistence after detection. An unproven
public-corpus assumption is not acceptable.
The full field, visibility, deletion and request-cost contracts are in
[`ARCHITECTURE.md`](ARCHITECTURE.md#youtrack).

Complete replacements synchronize title and citation-hint metadata as well as
the indexed bytes. Optional fields are normalized from the complete replacement,
never inherited from an older revision. Synchronization requires the fixed
`contentVisibilityContract: "visibility-intent/1"` capability before source
enumeration. REQUIRED success requires matching READY/current revision, mutation
item and intent, together with public denial of every historical route copy.

## Developing

From the repository root:

```bash
make connectors-test          # the whole suite
make connectors-gates         # local ruff + mypy + lock + the suite
```

or directly:

```bash
uv sync --directory connectors --all-extras
uv run --directory connectors pytest -q
```

Everything is one distribution (`ironrag-connectors`) with one lock file, one
Ruff/mypy/pytest configuration and one test run. A change to the framework is
verified against every source that uses it, in the same pipeline, before it
can merge.

## Adding a source

1. `src/ironrag_connector/sources/<name>/` — an adapter, a settings class, and
   whatever vendor client it needs.
2. Export a `PLUGIN = SourcePlugin(...)` from that package's `__init__.py`. Its
   `adapter_factory(settings, policies)` receives the live per-kind policy
   table; keep the reference only if the projection reads a policy field.
3. Add it to `_sources()` in [`registry.py`](src/ironrag_connector/registry.py).
4. Tests in `tests/sources/test_<name>_*.py` — unique basenames, no `__init__.py`.
5. A deployment template in `deploy/<name>/` and, if it ships as an image, a
   target in the [`Dockerfile`](Dockerfile) and a row in the release workflow.

The registry is a dict rather than a packaging entry point on purpose: a
string-keyed plugin registry is invisible to type checking and to the dead-code
gates, so a deleted registration would leave code that only looks used.

## Reference

- [`ARCHITECTURE.md`](ARCHITECTURE.md) — how each source maps its vendor's model onto IronRAG, and the invariants that must hold.
- [`CHANGELOG.md`](../CHANGELOG.md) — release history.
