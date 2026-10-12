# «Инфляция»: мониторинг цен ВкусВилла (Phase 2.1)

## Phase 2.1: активная сбалансированная корзина

Согласованная корзина B повторно проверена через официальный read-only MCP ВкусВилла и утверждена без замен. Активная версия — `basket-phase21-approved-42e79f43026a`, 42 товарные группы, стоимость 10 429 ₽. Её baseline за московскую неделю 2026-W41 полон (42/42), базовый индекс равен 100.

Историческая корзина A остаётся неизменной в `config/basket.toml`; её SHA-256 и все прежние наблюдения, ревизии и заблокированный baseline сохранены. Активный файл B хранится отдельно в `config/baskets/`, а SQLite содержит явный указатель на активную версию. Ряды A и B не объединяются.

```powershell
cargo run --quiet -- basket active
cargo run --quiet -- basket versions
cargo run --quiet -- baseline status
cargo run --quiet -- report inflation
cargo run --quiet -- report inflation --id basket-20261010T152005Z
cargo run --quiet -- weeks
cargo run --quiet -- weeks --id basket-20261010T152005Z
cargo run --quiet -- collect weekly
cargo run --quiet -- schedule export monday 09:00
```

Повторное утверждение точного согласованного черновика и повторный сбор внутри уже полного периода идемпотентны. Автоматические замены запрещены. Экспорт планировщика готов, но задача Windows не устанавливалась.

Главные материалы Phase 2.1: `research/phase21_report.md`, `phase21_approval_validation.json`, `phase21_basket_comparison.md`, `phase21_baseline_report.md`, `phase21_live_validation.md`.

## Phase 2: недельный мониторинг и аудит

Phase 2 добавляет независимый аудит семантики цены, версии корзины и методики, московские ISO-недели, неизменяемые ревизии снимков, индекс фиксированных физических количеств, структурированные логи и безопасный экспорт задания Windows.

Текущие статусы различаются намеренно:

- `EngineeringReady` — да;
- `WeeklyMonitoringReady` — да;
- исходная корзина — `BlockedByBasketAudit`;
- `BaselineReady` и `IndexPublicationReady` — нет.

Полный индекс не публикуется: банан в исходной корзине продаётся поштучно с ориентировочной массой 0,15 кг, а выбранный товар группы «Кофе» является кофейным напитком. Проверенный сбалансированный черновик сохранён отдельно и не утверждён автоматически.

```powershell
cargo run --quiet -- audit basket
cargo run --quiet -- audit report
cargo run --quiet -- basket compare
cargo run --quiet -- baseline status
cargo run --quiet -- collect weekly
cargo run --quiet -- weeks
cargo run --quiet -- report weekly
cargo run --quiet -- report promotions
cargo run --quiet -- schedule export monday 09:00
cargo run --quiet -- schedule status
```

`schedule export` только создаёт XML. Установка происходит исключительно по явной команде `schedule install`; в ходе Phase 2 системное задание не устанавливалось. Главные документы: `research/phase2_report.md`, `phase2_basket_audit.md`, `phase2_baseline_report.md`, `phase2_live_validation.md`.

Phase 1 — Rust-приложение для ограниченного еженедельного сбора цен из официального MCP ВкусВилла, ведения истории SQLite и фиксированной учебной продуктовой корзины. Используется анонимный контекст `Generic`: это цены официального каталога сети, но не подтверждённые цены конкретного магазина или адреса доставки.

Текущая утверждённая корзина содержит 42 продовольственные группы и стоит 9 932 ₽ при целевом бюджете 9 932,50 ₽ (¼ московского МРОТ 2026 года). После утверждения состав автоматически не меняется.

## Phase 1: запуск

```powershell
cargo run --quiet -- discover
cargo run --quiet -- products
cargo run --quiet -- basket build
cargo run --quiet -- basket show
cargo run --quiet -- basket approve
cargo run --quiet -- collect
cargo run --quiet -- history 100
cargo run --quiet -- export
cargo run --quiet -- stats
```

Настройки находятся в `config/settings.toml`, каталог — в `config/products.toml`, закреплённая корзина — в `config/basket.toml`, история — в `data/inflyaciya.sqlite3`, выгрузки — в `output/`. Нормативный источник московского МРОТ указан в настройках. Токены в проекте не хранятся.

Сбор ограничен 42 осмысленными запросами по товарным группам и пятью результатами на запрос. Это широкая воспроизводимая выборка, а не полный ассортимент. Инструменты покупок, реальной корзины и истории заказов запрещены allowlist клиента. При требовании учебной методики использовать именно физическую торговую точку понадобится отдельное согласование: текущий MCP не подтверждает такую привязку.

Phase 0 проверяет техническую доступность цен для магазина «Перекрёсток» в ТРК «Щука» (Москва, Щукинская улица, 42). Это исследовательский прототип, а не готовая система мониторинга.

Главный результат: САП-код магазина `B153` подтверждён официальным документом Перекрёстка, но идентификатор Купера и привязка цен к этому филиалу не подтверждены. Прямые запросы к Куперу получают `HTTP 403`, браузер показывает антибот-проверку. Защита не обходилась.

## Запуск

Из `C:\Game\Kras\Inflyaciya`:

```powershell
cargo run --quiet
```

Программа выполняет четыре низкочастотных HTTP-probe и импортирует `research/sample_products.json`. Результат печатается в JSON. Все наблюдения с неподтверждённым филиалом имеют `store_binding_confirmed: false`.

Можно передать другой локальный JSON того же формата:

```powershell
cargo run --quiet -- path\to\observations.json
```

## Проверки

```powershell
cargo fmt -- --check
cargo test
cargo clippy --all-targets --all-features -- -D warnings
```

## Содержимое

- `research/phase0_report.md` — подробный отчёт и решение о переходе к Phase 1.
- `research/source_capabilities.json` — машинно-читаемая матрица возможностей.
- `research/sample_products.json` — 28 реальных публично опубликованных наблюдений без подтверждённой привязки к B153; не корзина.
- `config/sources.toml` — целевая точка, источники и запрет использования неподтверждённой привязки.
- `src/main.rs` — probe, локальный импорт и нормализация через `rust_decimal`.

## Phase 0.6 — официальный MCP ВкусВилла

Добавлен минимальный Rust-клиент MCP Streamable HTTP для официального endpoint ВкусВилла. Анонимно подтверждены реальные товары, цены, категории, подробные карточки и скидки. Все такие цены классифицируются как `Generic`: связь с магазином на Маршала Василевского, 17 или адресом доставки не доказана. Авторизованная часть имеет состояние `PendingAuthorization`.

Примеры:

```powershell
cargo run --quiet -- --help
cargo run --quiet -- mcp ping
cargo run --quiet -- mcp tools
cargo run --quiet -- mcp search "молоко" 5
cargo run --quiet -- mcp details 36296
cargo run --quiet -- mcp discount 1
cargo run --quiet -- mcp repeat "молоко"
cargo run --quiet -- mcp auth-status
cargo run --quiet -- mcp sample research\phase06_sample_products.json
```

Клиент запрещает инструменты корзины, истории заказов и персонального «Любимого продукта». OAuth-регистрацию и вход он самостоятельно не выполняет; существующий краткоживущий access token можно передать только через `VUSVILL_MCP_ACCESS_TOKEN`, не записывая его в проект.

Результаты: `research/phase06_report.md`, `research/phase06_capabilities.json`, `research/phase06_sample_products.json`.

## Phase 0.5

Повторное исследование официального сайта Перекрёстка не дало ни одной цены с подтверждённой привязкой к B153. Выбран вариант **C — автоматический сбор не подтверждён**. Наиболее перспективен контролируемый ручной импорт кассовых чеков, наблюдений и действующих официальных каталогов с явным подтверждением магазина.

Новые артефакты:

- `research/phase05_report.md` — исследование официального сайта и решение C;
- `research/phase05_capabilities.json` — матрица проверенных способов;
- `research/phase05_sample_products.json` — 0 подтверждённых B153-цен и 5 явно отделённых неподтверждённых примеров схемы.

Проверка резервной схемы импорта:

```powershell
cargo run --quiet -- --phase05 research\phase05_sample_products.json
```

Валидатор не позволяет поместить запись в `confirmed_observations`, если она не содержит `store_id = "B153"`, адрес Щукинской, 42 и согласованную нормализацию цены.
