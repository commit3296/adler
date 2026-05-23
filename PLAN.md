# Adler — план разработки

OSINT-инструмент поиска юзернеймов по сайтам. Преемник Sherlock, написанный на Rust.

**Цели:** в 5–10× быстрее Sherlock, в 3× меньше false positive, data-driven сигнатуры, enrichment, корреляция.

---

## Фаза 0 — Bootstrap

- [x] `adler-core` (lib) создан вручную (без `cargo new`, чтобы не плодить лишнего git/файлов)
- [x] Workspace в корневом `Cargo.toml` (core + cli). `sites`/`tui`/`server` отложены до фаз, где они нужны
- [x] `rustfmt.toml` + workspace-level `[lints]` в `Cargo.toml` (отдельный `clippy.toml` не нужен — конфиг линтов через `[workspace.lints]`)
- [x] `.gitignore` (target, .env, *.db, CLAUDE.md)
- [x] `git init` + первый коммит
- [x] GitHub Actions: fmt + clippy + test на push/PR с `Swatinem/rust-cache`
- [x] `tracing` + `tracing-subscriber` подключены в `adler-cli` (через `ADLER_LOG` env); библиотека `adler-core` только эмитит, не инициализирует subscriber
- [x] `thiserror` + крейт-уровневый `Error` enum (`#[non_exhaustive]`) + `Result<T>` alias
- [x] README с roadmap-ссылкой на этот файл
- [x] MIT (`LICENSE`)

---

## Фаза 1 — MVP

**Цель: `adler <username>` работает на 50 сайтах быстрее Sherlock.**

### Core engine

- [x] `Site` struct + десериализация из JSON (`DetectionStrategy` enum: `status` / `body_marker` / `redirect_marker`)
- [x] `Username` newtype с валидацией (`[A-Za-z0-9._-]{1,64}`, serde-aware)
- [x] `UrlTemplate` с валидацией плейсхолдера и схемы
- [x] `CheckOutcome` + `MatchKind` (`Found` / `NotFound` / `Uncertain`)
- [x] Пропустил `Checker` trait — единственная реализация, добавим trait когда появится вторая (не YAGNI)
- [x] Базовая HTTP-стратегия: status / body marker / redirect marker
- [x] `tokio` + `reqwest` (HTTP/2 включён по дефолту в reqwest 0.12, connection pool тоже)
- [x] Конкурентный executor через `tokio::task::JoinSet` (`FuturesUnordered` отстаёт по эргономике от JoinSet, выбрал второе)
- [x] Per-request таймаут на уровне `ClientBuilder` (reqwest сам прерывает запрос); user-agent настраиваемый
- [x] Per-host rate limiter — самописный minimum-interval throttle (`HostThrottle`). `governor` отложен на Фазу 2 если понадобится burst-обработка
- [x] Общий deadline (`ExecutorOptions::deadline`) — сайты, не успевшие к дедлайну, выдают `Uncertain` с note "deadline reached"
- [x] Retry с экспоненциальным backoff + jitter — `RetryPolicy` в `Client`: max_retries=2 по умолчанию, base 500 ms, max 30 s, jitter ±25%. Triggers только на ban-помеченные Uncertain (rate_limited / cloudflare_challenge). Network errors не ретраятся
- [ ] Per-site headers / method — отложил до фазы 2 (не нужно для текущей схемы стратегий)

### Site registry

- [x] JSON-схема сайта — фиксирована в `adler-core/src/site.rs` через serde (отдельный JSON Schema файл — overkill для нашего размера, добавим в Фазу 5 если будут внешние contributors)
- [x] 15 сайтов в стартовом наборе (GitHub, GitLab, Bitbucket, Dev.to, CodePen, Keybase, SoundCloud, Patreon, Last.fm, Vimeo, Twitch, About.me, Replit, HackerNews, Reddit). 50 — после стабилизации схемы; markers/regex доточит `adler doctor` в Фазе 2
- [x] Валидация при загрузке: пустой список, дубликаты имён (case-insensitive), невалидные site definitions (через `Site::validate`). Внешний `jsonschema` крейт не нужен — serde уже валидирует структуру, остальное руками
- [x] Embed дефолтного реестра в бинарь через `include_str!("../data/sites.json")`
- [x] Override через `Registry::load_from_path` (CLI флаг `--sites` — в коммите 5)

### CLI

- [x] `clap` v4, derive API + `after_help` с примерами
- [x] `adler <username>` — базовый сценарий
- [x] `--format text|json|ndjson`
- [x] `--timeout`, `--concurrency`, `--only`, `--exclude`, `--deadline`, `--sites`, `--no-progress` (display-флаги `--all`/`-q`/`--color` доработаны в Фазе 4)
- [x] Прогресс-бар (`indicatif`) — автоматически выключается на `json`/`ndjson`, не-TTY stderr и в `--quiet`
- [x] Цветной TTY-вывод (ANSI escape) — управление через `--color` + `NO_COLOR` (Фаза 4)
- [x] Exit code: 0 если найдено хоть что-то, 1 если ничего, 2 при ошибке (валидация, registry, build client)
- [x] Tracing: `ADLER_LOG=adler=debug` env, вывод на stderr
- [x] Executor получил `run_with_progress(callback)` для стриминга прогресса

### Тесты

- [x] Unit-тесты для детекции через `wiremock` (status / body_marker / redirect_marker + network failure)
- [x] CLI integration-тесты (`assert_cmd` + wiremock + tempfile, 10 шт): help/version, exit codes (0/1/2), --only/--exclude/--found-only, json/ndjson форматы, end-to-end found
- [x] Snapshot-тест текстового вывода через `insta` с filters для нормализации времени и порта
- [x] Criterion microbench (`adler-core/benches/scan.rs`) — 3 уровня concurrency vs 50 mock-сайтов, ловит регрессии overhead
- [ ] Real-network nightly job — отложил, инфраструктура (отдельный workflow + ручной trigger чтобы не бить по реальным сайтам в каждом push)

### Гейт фазы

- [ ] Бенчмарк ≥5× vs Sherlock на 50 сайтах — требует реальной сети + установленного Sherlock; сделаю отдельным `scripts/bench-vs-sherlock.sh` и измерю один раз
- [x] `cargo clippy --all-targets -- -D warnings` чистый (pedantic + nursery)
- [x] Покрытие core ≥70% — **измерено `cargo-tarpaulin`: adler-core 727/780 ≈ 93%**. Overall 76% занижен из-за `main.rs` 0/242 (CLI-тесты запускают бинарь как subprocess через `assert_cmd` — tarpaulin их не инструментирует, но они его реально гоняют black-box)

---

## Фаза 2 — Надёжность

**Цель: меньше false positive, не банят, сигнатуры самодиагностируются.**

### Ансамбль детекции

- [x] Расширил схему: `Vec<Signal>` вместо `DetectionStrategy`. Strict-aggregation вместо score (любой Found-vote + 0 NotFound = Found; зеркально; иначе Uncertain). Полная миграция sites.json + всех тестов
- [x] Сигнал: финальный URL после редиректов (`RedirectAbsent`)
- [x] Симметричные сигналы для тела (`BodyPresent` / `BodyAbsent`) и статуса (`StatusFound` / `StatusNotFound`)
- [ ] Сигнал: baseline-сравнение — коммит 2
- [ ] Сигнал: длина ответа vs baseline (z-score) — коммит 2
- [ ] Сигнал: title hash — отложил (не критично для MVP качества; добавится когда найдём сайт, где остальное не работает)
- [x] Default semantics: contradicting / silent signals → Uncertain (более консервативно чем Sherlock; меньше false positive)
- [ ] `--strict` флаг — не нужен: текущий default уже строгий. Добавлю `--all-uncertain` если будет запрос
- [x] Body читается только если есть body-сигнал (skip-optimization)

### Self-healing

- [x] `adler --doctor` — для каждого сайта пробует `known_present` (если задан) и random 24-char nonsense username; ругается если known_present не Found или nonsense становится Found. Exit 0 если все OK, 1 если есть провалы
- [x] Поля `known_present` / `known_absent` в схеме сайта (optional)
- [x] Отчёт построчно: `[OK] / [FAIL]` + reasons; цветной TTY
- [x] `--fix` режим — `adler --doctor --fix`: для каждого упавшего сайта диффит present/absent ответы и **предлагает** сигнатуру (status различается → StatusFound/NotFound; иначе разные `<title>` → BodyAbsent). Только печатает готовый JSON-сниппет, ничего не меняет (реестр генерится импортёром). Не выводит для протухших known_present (ответы неразличимы)
- [x] CI nightly doctor (`.github/workflows/doctor.yml`, cron 04:00 UTC + manual): прогон по всем сайтам, классификация structural (rot) vs transient (бан/сеть). Job краснеет только на structural — CI-IP рейтлимиты не шумят. Полный отчёт в artifact + run summary. `--max-retries 0` чтобы баны всплывали сразу

### Защита от банов

- [x] Распознавание ban-сигналов: 429, 503+`Retry-After`, 502/503/520 с Cloudflare server header / `cf-ray`. In-body: «Just a moment...», «Checking your browser», `cf-browser-verification`. Без срыва на легитимные 403 (требуют явного `StatusNotFound[403]`)
- [x] Note становится machine-readable: `rate_limited` / `cloudflare_challenge` / `captcha`, видно в JSON/NDJSON выводе
- [x] Глобальный лимит RPS: `--max-rps <N>` (gate на все хосты, `Duration::from_secs(1)/rps`, композится с per-host throttle)
- [ ] Per-host rate limit в схеме сайта — отложил (YAGNI: ни один из 15 сайтов не требует кастомного интервала; добавление поля = churn в конструкторах ради нулевой пользы. Сделаю когда сайт реально потребует)
- [x] Proxy support: `--proxy <url>` (http/https/socks5 через reqwest + socks feature). Схема валидируется upfront (`http(s)://`/`socks5://`/`socks5h://`) — иначе reqwest молча трактует бесхемную строку как хост и все пробы тихо падают в Uncertain
- [x] Tor: `--tor` (preset socks5://127.0.0.1:9050, конфликтует с `--proxy`)
- [x] Ротация User-Agent: `--rotate-ua` (встроенный пул из 5 браузерных UA, random per-request)
- [ ] `--proxy-list` (ротация прокси) — отложил, нужен реальный кейс; одиночный `--proxy` покрывает базу

### Кэш

- [x] Файловый кэш (JSON), а не SQLite — пушбэк: кэш нужен только между запусками, доступ bulk (load→in-memory→save), конкурентных записей нет. SQLite/WAL — оверкилл и blocking-in-async. Переедем если сканы станут долгими и понадобится инкрементальная персистентность
- [x] Ключ `(site name, username)` + FNV-1a signature от `url`+`signals` (детерминированный, инвалидирует кэш при смене определения сайта) + TTL
- [x] `Uncertain` никогда не кэшируется (transient)
- [x] `--no-cache`, `--cache-ttl`, `--cache-path`, `--cache-clear`
- [x] Атомарная запись (temp + rename), создание родительских директорий, default путь `$XDG_CACHE_HOME/adler/` → `~/.cache/adler/`
- [x] Corrupt/missing файл → пустой кэш (никогда не роняет скан)

### Гейт фазы

- [~] False positive rate <5% — **измерено на курируемой выборке 13 сайтов с заведомо-известной истиной (реальные аккаунты + синтетический отсутствующий): 0% FP** (0 из 13 — Adler ни разу не сказал Found про несуществующий аккаунт). Точность Found-вердиктов 100%. Полный гейт «100×200» по-прежнему требует масштабного прогона, но на выборке сигнал сильный (см. «Валидация детекции»)
- [x] Поддержка 200+ сайтов — **416 сайтов** импортировано из Sherlock (`scripts/import_sherlock.py`), provenance/attribution в `data/sites.json` + README. Каждый сайт с `known_present` для doctor
- [ ] Прогон 1000 юзернеймов без банов с дефолтным rate limit — нужен реальный прогон

### Валидация детекции (резидентный оракул)

- [x] **Метод:** Browserbase (`proxies:true` → резидентный/мобильный IP, T-Mobile US) как независимый «оракул правды» — реальный браузер заходит на каноничные URL и фиксирует существование там, где сырой HTTP Adler'а упирается в login-wall/бан. Драйв через Playwright (`connectOverCDP`)
- [x] **Результат (выборка 13 сайтов):** FP **0%**; точность Found **100%**; recall **~62%** (8/13 существующих найдены, 5 → Uncertain, **ни одного ложного NotFound**). Оракул с резидентного IP чисто разрешил промахи (GitHub `torvalds`, Instagram `instagram`, X `x` — existing vs absent различимы)
- [x] **Корневая причина промахов — НЕ логика детекции, а:** (1) гнилые сторонние URL в реестре (наследие Sherlock: Instagram→`imginn.com`, Twitter→`nitter.privacydev.net`, Pinterest→`oembed.json`) флапают → Uncertain; (2) дата-центровый IP режется ботозащитой (TikTok/GitHub/HackerNews) — с резидентного IP резолвится. Консервативная агрегация деградирует в Uncertain, а не в ложный Found — отсюда нулевой FP
- [x] **`known_present="blue"` (186/416 сайтов) — это Sherlock'овский `username_claimed`, не мусор.** Оставлен нетронутым: удаление лишило бы doctor проверки 45% реестра. Чистить можно только *доказуемо* протухшие (через оракул/чистый IP), как уже сделано для Monkeytype в `OVERRIDES`
- [ ] **Дата-центровый прокси ≠ резидентный:** проверенный SOCKS-прокси (`AS202656 IT Hostline`) эмпирически НЕ снижал баны (Instagram/Twitter резались так же, Reddit даже хуже). Резидентность даёт только настоящий мобильный/домашний IP (Browserbase-пул) — единственный путь к достоверным цифрам по bot-protected сайтам
- [ ] **Полная валидация реестра** (416 сайтов) — требует резидентного прогона; через браузер дорого/медленно (per-page латентность), целесообразна выборка + точечный фикс гнилых URL

---

## Фаза 3 — Что отличает от всех

**Цель: enrichment + корреляция → реальный OSINT, а не just-checker.**

### Enrichment

- [x] Схема: `extract: Vec<Extractor>` на Site (field + CSS selector + опц. attr). Селекторы валидируются при загрузке реестра
- [x] `--enrich` флаг (обходит кэш — данные time-sensitive). Извлечение только для `Found` сайтов с extractor-правилами
- [x] Безопасный HTML-парсинг (`scraper` 0.23.1, пинн из-за clippy 1.86/let-chains), лимит парсинга 4 MB, значения trim + collapse + cap 512
- [x] GitHub extractors (name/bio/avatar) через импортёр OVERRIDES; вживую вытащил «Linus Torvalds» + avatar
- [ ] Топ-20 сайтов — пока только GitHub; остальные по мере необходимости (селекторы хрупкие, добавлять с проверкой)
- [ ] Скачивание аватарок в папку — отложил до коммита correlation (там понадобятся для perceptual hash)

### Корреляция

- [x] Bio similarity (нормализация + токенизация + Jaccard) + name (exact normalized / token Jaccard)
- [x] Кластеризация через union-find: пары со score ≥ `LINK_THRESHOLD`(0.5) линкуются; confidence = средний score рёбер
- [x] `--correlate` (implies `--enrich`, обходит кэш); текстовый вывод: кластеры + confidence + shared name, unlinked, no-profile
- [x] Confidence score (эвристика триажа, не доказательство)
- [ ] Perceptual hash аватарок — отложил: `image`/`image_hasher` тянут тяжёлое дерево + риск clippy 1.86 (как scraper); exact-URL аватарок межсайтово бесполезен. Добавлю опционально если разрешится toolchain
- [ ] Граф JSON + Graphviz DOT — отложил в P3.4 (report), где структурный вывод уместен

### Permutation engine

- [x] `permute(username, level)` модуль: basic (separator swaps `_`/`-`/`.`/removal), aggressive (basic + leet по одному классу + суффиксы `1`/`123`)
- [x] Leet: `o→0 i→1 e→3 a→4 s→5`, по одному классу за раз (не комбинаторно — реалистичнее и bounded)
- [x] `--permute none|basic|aggressive` (CLI enum → core `PermuteLevel`, ядро не зависит от clap)
- [x] Дедупликация, оригинал всегда первый, все варианты валидируются как Username, cap `MAX_VARIANTS=64`
- [x] CLI расширяет юзернейм и сканирует каждый вариант; URL в выводе показывает какой

### Reporting

- [x] `--format html`: self-contained отчёт — Found-карточки с enrichment + превью аватарок (`<img>` на origin), секция correlation (если `--correlate`), свёрнутый `<details>` для not-found/uncertain
- [x] Без template-движка — ручная разметка со строгим HTML-экранированием (защита от XSS/поломки), CSS inline. Обоснование: одна статичная разметка + риск clippy-1.86 несовместимости у `askama`/`tera`
- [ ] Граф связей (DOT) — отложил, текущий список кластеров покрывает потребность; DOT добавлю если будет запрос

### Гейт фазы

- [ ] Enrichment работает на ≥20 сайтах — пока 1 (GitHub); движок готов, дело за data-work (добавить extractors с проверкой). Не блокер функциональности
- [x] HTML-репорт пригоден к отправке: валидный self-contained HTML, проверен вживую на GitHub (Linus Torvalds + avatar)

---

## Фаза 4 — UX

### TUI

- [x] `ratatui` 0.30 + `crossterm` 0.29 (проверил совместимость с clippy 1.86 — собирается)
- [x] `--tui`: интерактивный браузер результатов. Список с цветами по вердикту, drill-down (`Enter`: url/note/why/enrichment), `j/k`/стрелки, `q`/`Esc`
- [x] Дефолтный фильтр **found+uncertain** (прячет шум NotFound, как у текста); `f` циклит found+uncertain → all → found → not found → uncertain. Заголовок: счётчики вердиктов + позиция `[pos/total]`
- [x] Навигация по практикам vim/less/lazygit/k9s: `/` инкрементальный поиск (site/url, комбинируется с фильтром); `g`/`G`/`Home`/`End` верх/низ; `PageUp`/`PageDown`; `?` оверлей со всеми клавишами
- [x] Действия: `o` открыть URL в браузере (крейт `open`, с фидбэком успеха/ошибки в footer), `y` копировать URL и `Y` — все найденные через **OSC 52** (без GUI-clipboard-зависимости, по SSH; самописный base64). Side-effects в `Action` enum — `App`/`handle_key` чисты
- [x] **Master-detail в две панели** на широких терминалах (≥90 кол.: список + деталь выбранного постоянно справа, как lazygit/k9s); узкие — одна панель, деталь по `Enter`
- [x] **Живой стриминг**: скан идёт в фоне (отдельная task шлёт исходы в канал), TUI поллит клавиши + дренит канал, результаты прилетают вживую, индикатор «scanning…» в заголовке; выход из TUI прерывает скан. Кэш в live-режиме обходится
- [x] Тестируемость: `App` + `render` юнит-тестятся через `TestBackend` (18 тестов: фильтры/навигация/поиск/help/copy-all/split/live-push/статус); `run_live` event-loop и I/O (`open`/OSC52) — compile-verified, **интерактивно в этом окружении не прогонялись**
- [x] Guard: `--tui` без TTY → внятная ошибка
- [ ] Экспорт из TUI — отложил (есть `--format html/json/csv` + `Y` копирует URL); добавлю если будет запрос

### CLI polish

- [x] Shell completions: `--completions <shell>` (bash/zsh/fish/PowerShell/elvish через `clap_complete`)
- [x] `adler --help` с примерами (`after_help`)
- [x] `--list-sites` — перечисляет имена сайтов реестра (с учётом `--only`/`--exclude`); поиск среди 416 для подбора фильтров
- [ ] `--verbose` — `ADLER_LOG` env уже покрывает (tracing); отдельный флаг добавлю если попросят
- [ ] `adler update-sites` — Фаза 5 (нужен hosted реестр + версионирование)

### UX по современным практикам (clig.dev / ripgrep / Evil Martians)

- [x] Сигнал/шум: текст по умолчанию показывает Found+Uncertain, прячет (обычно сотни) NotFound; `--all` для полного списка. Заменил инвертированный `--found-only`. Tally считает всё, HTML-отчёт остаётся полным (свёрнутый `<details>`)
- [x] Live-стриминг: на интерактивном TTY результаты печатаются по мере нахождения (через колбэк executor'а, согласовано с прогресс-баром через `suspend`), как у ripgrep. Пайп/JSON/NDJSON/HTML/TUI — батч с детерминированным порядком
- [x] `--color auto|always|never` + `NO_COLOR` (no-color.org); auto красит только интерактивный stdout. Применено к скану и doctor
- [x] `-q`/`--quiet`: только URL найденных, по одному в строке; без прогресса/сводки/подсказок. Для скриптов
- [x] Next-steps подсказка после интерактивного скана (`--enrich`/`--tui`/`--format json`), скрыта в пайпе и quiet
- [x] Внутренности вывода переразложены в `print_row`/`print_tally`/`should_show`/`DisplayOpts` — общие для стриминга и батча; `run()` тонкий диспетчер, `run_scan` владеет оркестрацией

### Документация

- [x] rustdoc на 100% публичного API core (`RUSTDOCFLAGS=-D warnings cargo doc` чистый — missing_docs + broken-link checks)
- [x] Примеры использования как библиотеки (runnable `no_run` doctest в crate-доке)
- [x] README с полным usage по всем CLI-фичам (фильтры, форматы, enrich/correlate/permute, proxy/Tor/UA, cache, completions, doctor/fix)
- [ ] `mdbook` сайт — отложил в Фазу 5 (контент + внешний тул, не код)

---

## Фаза 5 — Экосистема

- [~] Релиз 1.0 на crates.io — метаданные готовы (keywords/categories/docs/binstall), `cargo publish -p adler-core --dry-run` зелёный. Блокеры: реальный repo URL (placeholder) + сама публикация (порядок: core → cli)
- [x] Готовые бинари в GitHub Releases: `.github/workflows/release.yml` (5 таргетов: linux/macos/windows × x86_64/arm64) по тегу `v*` через `taiki-e/upload-rust-binary-action`. Не прогонялся (нужен реальный tag-push)
- [x] `cargo binstall` поддержка: `[package.metadata.binstall]` в adler-cli, pkg-url совпадает с артефактами release.yml
- [x] Docker image: multi-stage `Dockerfile` (rust-bookworm builder → debian-slim + ca-certs), **собран и проверен вживую** (`adler 0.1.0`, реальный скан GitHub в контейнере, 91 MB). Multi-arch buildx — в release-пайплайне при необходимости
- [ ] Homebrew formula / AUR — отложил (нужен реальный release + контрольные суммы артефактов; шаблоны добавлю после первого тега)
- [ ] `adler-sites` отдельный репо — отложил (текущий embed достаточен; вынесем когда появятся внешние contributors)
- [ ] HTTP API (`adler serve`) + Web UI — отложил (опционально по PLAN; web-фреймворк = toolchain-риск, отдельный скоуп)

---

## Фаза 6 — Охват и таргетинг (рост ценности через метаданные)

**Идея:** сайты различаются по категории и региону; пользователю полезнее
прогон по релевантной подвыборке, а не по всем 416. Реестру нужны
структурированные метаданные.

- [x] **Теги сайтов** — поле `tags: Vec<String>` в схеме (generic, не жёсткие
  category/region; `axis:value` — лишь конвенция). CLI `--tag` (повторяемый,
  ИЛИ внутри запрошенных, untagged исключается при фильтре) + `--list-tags`
  (теги с счётчиками). Эвристика стартового набора в импортёре
  (`derive_tags`: ccTLD→`region:xx` + curated CATEGORY_MAP + регион-bound
  платформы); применено к текущему реестру через `scripts/tag_sites.py`
  (82/416 размечены). JSON Schema обновлена
- [x] **Бот-защищённые помечены тегом `bot-protected`** (Instagram/Snapchat/
  TikTok/Twitter — из валидации) вместо нового поля схемы. `--exclude-tag
  bot-protected` для быстрого чистого прогона, `--tag bot-protected` чтобы
  увидеть, какие требуют резидентного IP/браузера. Добавлен `--exclude-tag`
- [ ] **Browser-backend** (Browserbase/Playwright) для `requires_residential`
  сайтов — реальный браузер с резидентного IP вместо raw-`reqwest`. **Прямо
  бьёт по главному разрыву (recall бот-защищённых).** Тяжёлый, опциональный
  (feature/отдельный крейт)
- [x] **`adler --watch [--interval N]`** — мониторинг: свежий скан, дифф
  found-сета против снапшота (`<cache>/watch/<user>.json`), репорт
  new/removed аккаунтов, сохранение нового снапшота; `--interval` —
  непрерывный цикл (иначе one-shot под cron). Чистая `diff_found`
  (юнит-тесты) + тонкая loop/IO-оболочка; снапшот переиспользует serde
  у `CheckOutcome`
- [x] **Batch-режим `--input users.txt`** — много юзернеймов за прогон
  (одна на строку, `#`-комменты и пустые пропускаются, дедуп; позиционный
  тоже включается). Общий кэш, вывод сгруппирован по юзернейму для
  text/json/ndjson; `--quiet` даёт `username\turl`. Несовместим с
  `--tui`/`--correlate`/`--format html`. Переиспользует `scan_one`+executor
- [ ] **`--top N` / тиры по популярности** — быстрый первый проход по
  N релевантным сайтам (поле `tier`/`popularity` в метаданных), потом `--all`
- [x] **Explainability вердикта** — `CheckOutcome.evidence` несёт описания
  сработавших сигналов («HTTP 404 (status_not_found)»). `--explain` печатает
  их под каждым результатом (text), JSON/NDJSON всегда включают, TUI detail
  показывает. Данные из per-signal eval, без дублирования логики
- [x] **CSV-экспорт** — `--format csv`: плоская таблица с заголовком
  (`site,url,kind,reason,elapsed_ms,evidence`; в batch добавляется колонка
  `username`). RFC 4180-квотирование захардкожено (без крейта).
- [ ] **Граф/Maltego экспорт** — нишевее, отложено до запроса

---

## Этика и безопасность

- [x] `--respect-robots` — opt-in; per-origin кэш `robots.txt` (дедуп fetch), минимальный парсер (Disallow-префиксы для группы `adler`→`*`, Allow игнорируется = консервативнее). Disallowed-путь → `Uncertain` note `robots_disallowed` без запроса. Missing/unreadable robots → allow
- [x] README раздел «Ethics & responsible use»: legitimate vs prohibited use + принцип «detect, never circumvent»
- [x] `--audit-log <path>` — NDJSON-запись (ts/username/site/url/kind) на каждый результат, append-режим
- [x] `CODE_OF_CONDUCT.md` (Contributor Covenant 2.1)
- [x] `SECURITY.md` (приватный репорт + supported versions + no-circumvention principle)
- [x] Никаких captcha-bypass / Cloudflare-обхода в core — зафиксировано как принцип в SECURITY.md; код уже только детектит (ban-модуль → Uncertain), не обходит

---

## Доводка по функциональному прогону

Ручной прогон приложения по матрице данных/конфигураций (форматы text/json/
ndjson/csv/html, фильтры, `--all`/`--quiet`/`--explain`/цвет, enrich/permute/
input/doctor/cache/audit-log/add-site, edge-кейсы, `--max-rps`/concurrency).
Поведение корректно по всей матрице; найдены и исправлены два дефекта:

- [x] **Двойная печать источника** у `Error::Io`/`Json`: `#[error("…: {0}")]`
  + `#[from]` заставлял anyhow `{:#}` печатать вложенную ошибку дважды. Убран
  `{0}` — слой называется в сообщении, деталь даёт цепочка (один раз)
- [x] **Валидация схемы `--proxy`** upfront (см. Фаза 2) — бесхемный URL
  больше не уходит молча в Uncertain, а сразу даёт понятную ошибку
- [x] Заодно подтверждено вживую: `--doctor` честно ловит too-permissive
  сигнатуры (reddit отдаёт 200 на несуществующих), enrich тянет реальные
  поля (GitHub → «Linus Torvalds»), `--max-rps` реально спейсит запросы

---

## Метрики успеха

- [ ] ≥5× быстрее Sherlock на одинаковых сайтах
- [ ] <5% false positive
- [ ] ≥300 сайтов в реестре
- [ ] ≥500 stars на GitHub за 6 мес после 1.0
- [ ] Хотя бы один внешний contributor мерджит сигнатуру сайта
