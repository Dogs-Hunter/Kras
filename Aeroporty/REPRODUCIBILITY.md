# Воспроизводимость Aeroporty 1.0.0

Финальный выпуск датирован 2026-10-12. Готовые данные и их SHA-256 находятся в `output/release/manifest.json`.

## Два разных сценария

1. **Воспроизведение зафиксированного релиза** выполняется только из сохранённых локальных snapshots и не обращается к сети.
2. **Обновление источников** командой `update` получает актуальные на момент запуска данные. Получившийся набор может отличаться от релиза 1.0.0 и не считается побитовым воспроизведением старого выпуска.

## Локальные файлы, необходимые для полного offline-прогона

Эти файлы сохранены в рабочей копии, но исключены из обычного Git из-за размера или статуса сторонних материалов:

- `cache/airports.csv`, `cache/runways.csv`, `cache/countries.csv` и их `.meta.json`;
- `cache/faa/2026-10-01/` — архив и извлечённые FAA/NASR APT tables с manifest;
- `cache/anac/2026-10-11/` — закреплённые ANAC Public/Private V2, SIROS и исторические таблицы;
- `cache/anac-research/2026-10-11/` — независимый JSON-аудит Public V2;
- `cache/evidence/phase5/` — официальные PDF/XML, использованные при разрешении восьми конфликтов;
- `output/candidate-phase1/airports.json`;
- `output/candidate-phase2/airports.json`;
- `output/candidate-phase3/airports.json`, `report.json` и `rejected.csv`;
- `output/candidate-phase4/` — неизменяемый baseline предыдущей фазы.

Хэши FAA/ANAC и подтверждающих документов находятся в manifests соответствующих каталогов. SHA-256 всех 21 файлов Phase 4 закреплены в `data/phase5_baseline.json`. Phase 5 прекращает работу, если baseline или evidence cache изменены.

Для долговременного хранения следует передать перечисленные каталоги отдельным архивом вместе с проектом либо сохранить их в доступном преподавателю файловом хранилище. Без этого архива Git-репозиторий остаётся полностью пригодным для сборки, тестирования, проверки и чтения готового релиза, но не для полного offline-пересчёта исходных данных.

## Команды

```powershell
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test
cargo build --release
cargo run --release -- run --offline
cargo run --release -- release
cargo run --release -- validate --output output/release
```

`release` заново экспортирует данные из проверенного `candidate-phase5`, сверяет CSV/JSON/TOML/YAML и атомарно заменяет `output/release/` только после успешного аудита.

## Получение обновлённых источников

```powershell
cargo run --release -- update --faa-release 2026-10-01 --anac-release 2026-10-11
```

Для реального обновления следует указать существующие новые версии источников. Не следует ожидать совпадения SHA-256 с релизом 1.0.0 после обновления OurAirports или регуляторных реестров.

