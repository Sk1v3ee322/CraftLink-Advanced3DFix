# FilmCraft ↔ EffectCraft Dynamic Link & Timeline Interoperability

> ⚠️ **УСТАРЕЛО / DEPRECATED.** Этот документ описывает первую версию моста и содержит
> идентификаторы команд, которых больше нет (`clip.replaceWithEffectCraft`,
> `clip.sendToEffectCraft`, порт `9877`). Актуальная реализация — **CraftLink**; её описание,
> форматы обмена, таблицы файлов/функций/команд и порядок сборки находятся в
> [`CRAFTLINK_ARCHITECTURE.md`](CRAFTLINK_ARCHITECTURE.md). Ниже документ сохранён только как
> история.
>
> ⚠️ **DEPRECATED.** This file describes the first version of the bridge and names command IDs
> that no longer exist (`clip.replaceWithEffectCraft`, `clip.sendToEffectCraft`, port `9877`).
> The current implementation is **CraftLink**; see
> [`CRAFTLINK_ARCHITECTURE.md`](CRAFTLINK_ARCHITECTURE.md) for its architecture, interchange
> formats, file/function/command tables and build steps. The text below is kept for history only.

> **Документация для разработчиков и языковых моделей (LLM)**  
> **Дата обновления:** 14 ноября 2026 г.  
> **Статус:** Реализовано, протестировано, исправлены замечания по консоли и серому фону, скомпилировано в оптимизированные `--release` бинарники.

---

## 1. Обзор архитектуры (Executive Summary)

Реализован механизм межпрограммного взаимодействия (Dynamic Link / Timeline Interchange) между видеоредактором **FilmCraft** (аналог Adobe Premiere Pro) и средой композитинга и визуальных эффектов **EffectCraft** (аналог Adobe After Effects). Обе программы разработаны на чистом Rust.

### Основные возможности:
1. **Выделение клипов на таймлайне FilmCraft:**
   - Пользователь выделяет один или несколько эпизодов/клипов на таймлайне FilmCraft (видео и аудио дорожки).
   - По правому клику мыши (контекстное меню клипа) или через меню `Clip` доступны команды:
     - **«Replace with EffectCraft Composition»** (`clip.replaceWithEffectCraft`) — заменяет выделенные клипы на таймлайне FilmCraft вложенным клипом-композицией (Nest) и передает исходные клипы со всеми таймкодами, свойствами и дорожками в EffectCraft.
     - **«Send to EffectCraft…»** (`clip.sendToEffectCraft`) — неразрушающий экспорт выделенных клипов в проект EffectCraft без изменения текущего таймлайна FilmCraft.
2. **Автоматическая передача и открытие в EffectCraft:**
   - Выделенные эпизоды нормализуются по времени (смещение к тику 0) и экспортируются в индустриальный формат обмена **Final Cut Pro 7 XML** (`<xmeml>`).
   - Механизм IPC автоматически проверяет, запущен ли EffectCraft с активным локальным каналом управления (JSON-lines TCP на `127.0.0.1:9877` или по порту из `effectcraft_control.json`).
   - Если EffectCraft запущен — команда `file.importTimeline` отправляется по сети мгновенно, и композиция открывается без перезапуска программы.
   - Если EffectCraft закрыт — запускается процесс `effectcraft.exe <path_to_xml>`.
3. **Прямое открытие XML таймлайнов в EffectCraft:**
   - EffectCraft CLI и GUI автоматически распознают переданные файлы таймлайнов (`.xml`, `.otio`, `.edl`, `.fcpxml`, `.aaf`, `.omf`) как на этапе старта программы, так и при вызове `file.import` (включая Drag-and-Drop).
4. **Обратная связь (EffectCraft → FilmCraft):**
   - В EffectCraft добавлена команда `file.sendToFilmCraft`, позволяющая отправить активную композицию обратно в FilmCraft через предрендер (ProRes 4444 с альфа-каналом) и XML таймлайн.

---

## 2. Разбор и устранение обнаруженных проблем

### Проблема 1: Открывающееся окно консоли (Terminal / Console Window)
* **Причина:**  
  В Windows подсистема бинарного файла задается PE-заголовком `Subsystem`. При сборке в dev-режиме (`cargo build` без `--release`) действовал атрибут `not(debug_assertions)` в `#![cfg_attr(all(target_os = "windows", not(debug_assertions)), windows_subsystem = "windows")]`, из-за чего PE-заголовок имел значение `CONSOLE` (3) вместо `WINDOWS` (2). При старте процесса Windows выделяла черное консольное окно для stdout/stderr. Также дочерний процесс при обычном `spawn()` наследовал консоль.
* **Исправление:**
  1. В `apps/filmcraft/src/main.rs` и `apps/effectcraft/src/main.rs` директива изменена на безусловную:
     ```rust
     #![cfg_attr(target_os = "windows", windows_subsystem = "windows")]
     ```
  2. При порождении процесса в `crates/engine/src/effectcraft_bridge.rs` добавлены флаги создания Windows-процесса:
     ```rust
     #[cfg(windows)]
     {
         use std::os::windows::process::CommandExt;
         const CREATE_NO_WINDOW: u32 = 0x08000000;
         const DETACHED_PROCESS: u32 = 0x00000008;
         cmd.creation_flags(CREATE_NO_WINDOW | DETACHED_PROCESS);
     }
     ```
  3. Обе программы скомпилированы в профиле `--release`. Консольное окно полностью отключено на уровне ОС.

---

### Проблема 2: Серый фон в окне просмотра EffectCraft
* **Причина:**  
  Встроенный демонстрационный проект FilmCraft (`Misty_Forest.mp4` и `Neon_Loop.mov`) использует процедурные шейдерные генераторы `filmcraft_media::Generator::Demo(...)` вместо физических видеофайлов на жестком диске.  
  При выгрузке в FCP7 XML они сериализовались в `<generatoritem>` с метаданными `<effectid>filmcraft:{"Demo":"Forest"}</effectid>`.  
  В модуле импорта EffectCraft (`effectcraft-interchange/src/import.rs`, строки 472–475) для всех неподдерживаемых генераторов задано стандартное поведение:
  ```rust
  _ => {
      self.warn(format!("generator \"{}\" was imported as a grey solid", g.label()));
      [0.5; 3] // сплошной 50% серый цвет RGB [0.5, 0.5, 0.5]
  }
  ```
  В результате EffectCraft создавал слой `Solid` серого цвета вместо видеофутажа.
* **Исправление:**  
  В `crates/engine/src/effectcraft_bridge.rs` добавлена процедура автоматической материализации генераторов `materialize_generators_for_export`:
  1. Перед выгрузкой в XML анализируются клипы передаваемой последовательности.
  2. Если клип ссылается на `Generator::Demo(scene)`, встроенный энкодер FilmCraft (`filmcraft_export::export`) за доли секунды запекает кадры сцены в реальный MP4-видеофайл (`%TEMP%/FilmCraft_Demo_<Scene>.mp4`).
  3. Временная последовательность перелинковывается на созданный физический MP4-файл (`MediaRef::File`).
  4. В FCP7 XML теперь формируется полноценный блок:
     ```xml
     <clipitem>
       <name>Misty_Forest.mp4</name>
       <file>
         <pathurl>file://localhost/C:/Users/.../AppData/Local/Temp/FilmCraft_Demo_Forest.mp4</pathurl>
       </file>
     </clipitem>
     ```
  5. EffectCraft импортирует его как `LayerSource::Footage`, подключает нативный декодер H.264/AAC и отображает полноценный живой видеоряд.
  6. Для любых реальных пользовательских файлов (видео с камеры, экрана и т.д.) они уже являются `MediaRef::File` и сразу передаются как футажи.

---

## 3. Полный реестр изменений по файлам

### Репозиторий 1: `FilmCraft` (`filmcraft-0.20.1`)

#### 1. `crates/engine/src/effectcraft_bridge.rs` (Создан)
* Логика выделения, запекания генераторов в MP4, формирования FCP7 XML, отправки по TCP Control Channel и запуска `effectcraft.exe` в скрытом режиме (`CREATE_NO_WINDOW | DETACHED_PROCESS`).

#### 2. `crates/engine/src/effectcraft_bridge_tests.rs` (Создан)
* Модульные тесты функционала моста (`bridge_fails_when_no_clips_selected`, `replace_with_effectcraft_creates_xml_and_replaces_on_timeline`, `send_to_effectcraft_keeps_original_timeline`).

#### 3. `crates/engine/src/lib.rs`
* Зарегистрирован публичный модуль `pub mod effectcraft_bridge`.

#### 4. `crates/engine/src/commands.rs`
* Зарегистрированы команды движка:
  * `clip.replaceWithEffectCraft`
  * `clip.sendToEffectCraft`

#### 5. `crates/engine/src/clip_ops.rs`
* Новые команды добавлены в реестр операций над клипами после `source.overwrite`.

#### 6. `crates/ui-egui/src/panels/timeline/menus.rs`
* В массив контекстного меню клипа `CLIP_MENU` добавлены:
  * `("Replace with EffectCraft Composition", "clip.replaceWithEffectCraft")`
  * `("Send to EffectCraft", "clip.sendToEffectCraft")`

#### 7. `apps/filmcraft/src/main.rs`
* Директива `#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]` гарантирует запуск без окна консоли.

---

### Репозиторий 2: `EffectCraft` (`effectcraft-0.24.1`)

#### 1. `apps/effectcraft/src/main.rs`
* Добавлена функция `is_timeline_file` и автоимпорт переданных файлов таймлайнов (`.xml`, `.otio`, `.edl`) при старте через CLI.
* Директива `#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]` для подавления консоли.

#### 2. `crates/engine/src/cmd/file.rs`
* В команду общего импорта `file.import` добавлено распознавание файлов таймлайнов и перенаправление в `file.importTimeline` (работает Drag-and-Drop XML прямо в окно программы).

#### 3. `crates/engine/src/cmd/timeline.rs`
* Добавлена команда `file.sendToFilmCraft` для отправки композиции обратно в FilmCraft.

#### 4. `crates/engine/src/tests/timeline_interchange.rs`
* Добавлен тест `send_to_filmcraft_exports_xml`.

---

## 4. Размещение скомпилированных файлов

Готовые оптимизированные исполняемые файлы (`--release`) копируются в портативные
каталоги рядом с распакованным релизом:

* `<портативный каталог FilmCraft>\filmcraft.exe`
* `<портативный каталог EffectCraft>\effectcraft.exe`

*(Оригинальные файлы сохраняются рядом с расширением `.exe.backup`.)*