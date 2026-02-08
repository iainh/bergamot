# E2E Test Gaps

Tracking file for end-to-end test coverage deficiencies to address before production readiness.

## Existing E2E Tests

- [x] `end_to_end_append_download_flow` — basic append + download + file verification
- [x] `missing_article_falls_back_to_second_server` — multi-server fallback
- [x] `crash_recovery_resumes_download` — disk state persistence and resume
- [x] `rpc_status_schema_conformance` — `status` response field validation
- [x] `rpc_version_reports_compatibility` — `version` >= 26 for Sonarr/Radarr
- [x] `rpc_listgroups_schema_during_download` — `listgroups` response fields during download

## RPC API Coverage Gaps

- [x] **`history` schema conformance** — Validate all fields required by Sonarr/Radarr after a completed download
- [ ] **`editqueue`** — Queue manipulation: pause/resume/delete/move/priority changes
- [ ] **`pausedownload`/`resumedownload`** — Downloads actually pause and resume
- [ ] **`rate`** — Speed limiting takes effect during download
- [ ] **`config`/`saveconfig`** — Round-trip config read/write; Sonarr uses `config` to discover paths
- [ ] **`servervolumes`** — Bytes-transferred accounting after download
- [ ] **`postqueue`** — Post-processing queue visibility during/after download
- [ ] **`scan`** — NZB dropped into `NzbDir` gets auto-picked up
- [ ] **XML-RPC** — All current tests use JSON-RPC; XML-RPC is a separate untested code path

## Functional Scenarios

- [ ] **Authentication rejection** — Wrong credentials get a 401
- [ ] **Multi-file NZB** — Multiple distinct output files produced correctly
- [ ] **Concurrent downloads** — 2+ NZBs appended simultaneously both complete without corruption
- [ ] **Post-processing pipeline** — PAR2 verify/repair + archive extraction as full e2e flow
- [ ] **Feed polling** — RSS feed pointing at stub HTTP server triggers auto-fetch
- [ ] **Extension script execution** — Post-processing script runs, exit code affects final status
- [ ] **Graceful shutdown under load** — Shutdown while downloading, disk state consistent for recovery
- [ ] **Error propagation** — Invalid NZB, corrupt yEnc, all-servers-down produce correct error statuses
