# Wex — Material 3 Expressive

**Интерфейс Wex Studio** развивается с дизайном и
анимациями Material 3 Expressive. Это относится к Rust/GPU-интерфейсу Wex,
а не требует добавлять Compose/Material в каждое редактируемое приложение.

## Основа

- Семантические роли: surface/container, primary/on-primary, selection,
  error/success. Код и текст сохраняют читаемость и контраст.
- Выразительная типографическая иерархия, мягкие формы, сгруппированные действия;
  Run — основное заполненное действие, устройство расположено рядом.
- Spatial-анимации используют прерываемую пружину, продолжающуюся из текущей
  позиции/скорости. Color/opacity effects ограничиваются допустимым диапазоном.
- Системные операции остаются асинхронными; анимация не ждёт компилятора или ADB.
- Reduced motion мгновенно устанавливает целевое состояние без подпрыгивания:
  переключатель Reduce motion и `WEX_REDUCED_MOTION=1`.

Компоненты и текущая реализация описаны в разделе «Цельный интерфейс» ниже.
Параметры пружины — токены Wex, не копия всех платформенных токенов Google.

Первичные источники:
[Material 3](https://m3.material.io/),
[Expressive motion physics](https://m3.material.io/styles/motion/overview/how-it-works).

## Цельный интерфейс — этап 5

Реализованы общая responsive app bar, navigation rail, rounded surface panels,
дерево проекта с compact package folders и поиском, настоящие файловые вкладки,
breadcrumb, виртуализированные редактор и вывод, segmented Logcat/Build/Problems,
chooser устройств с model/serial/state, Preferences, completion и Save/Discard/Cancel.
Основные действия остаются видимыми при ширине от 760 logical px; дополнительные
действия K2/model находятся в Preferences. Изменённые документы сохраняют Rope,
undo/redo и cursor при переключении, Build/Run сохраняют все открытые документы.

`src/material.rs` содержит spring physics и три motion token: fast spatial,
default spatial, effects. `src/material_ui.rs` содержит semantic palette,
typography, vector icons, badges, loading, buttons и keyed motion store.
`src/bin/ide/ui.rs` собирает интерфейс из этих компонентов. Положение/размер
индикаторов, раскрытие панелей/chooser и форма кнопок продолжают текущие position
и velocity при смене цели. Цвета ограничены диапазоном; reduced motion обнуляет
velocity, мгновенно устанавливает цели и отключает движение loading dots.
Native build, ADB и K2 не выполняются в UI-потоке.

Проверка живого окна: `wex-ide PROJECT --ui-smoke OUTPUT_DIRECTORY` открывает
настоящий winit/wgpu viewport и подаёт pointer/key events через eframe raw-input
hook, проверяя реальные hitboxes, focus, popup/modal layers и взаимодействия.
Она не нажимает Build/Run и не устанавливает APK. PNG снимаются самим renderer;
JSON сохраняет результаты, размеры viewport и motion samples. Отдельные unit
проверки покрывают 760/1000/1440 px, Enter activation, dirty tabs и external-write
conflicts. Это не замена ручному UX-аудиту или полному accessibility certification.

LSP-снимки используют монотонный `snapshot_clock` сессии: быстрый переход
A → B → A может схлопнуться в watch channel, но версия открытого K2-буфера
не уменьшается при возврате к parked/reopened Document. Отдельный regression
проверяет переключение, discard/reopen и сохранность in-memory текста.


## Редактор — этап 6

Find/Replace разворачивается в rounded surface card с прерываемой keyed spring
`find-height`; Aa/Word/regex/Selection используют общие tonal buttons. Breadcrumb
показывает Ln/Col и размер выделения, а в узком окне сохраняет поиск как icon action.
Область вывода адаптивно уступает место find/review controls, оставляя код видимым
при минимальном viewport760×560. Inline external-change card и confirm reload
используют error roles и тот же component system, а не отдельные стандартные виджеты.
Код, search hits, выделение и подчёркнутый IME preedit имеют разные семантические роли.
Caret и hit-testing используют позиции shaped Galley, не фиксированную ширину символа.
Длинный документ рисует только видимые строки; drag selection может прокручивать
его за краями viewport. Reduced motion распространяется и на find/review actions.

`--editor-smoke` проверяет real native window: мышь/клавиатура, OS clipboard,
события IME и candidate rect, поиск/замену, конфликты, parked/recreated files и
виртуализацию10,000 строк. Native eframe input automation не является проверкой
конкретного OS language service или полным accessibility audit.

## Отладчик — этап 7

Debug — общий tonal action рядом с Run, отдельный пункт rail и четвёртый сегмент
output dock. Сегменты адаптируются к ширине, а индикатор продолжает существующую
прерываемую spring-анимацию. Dock отдаёт больше места стеку/переменным, сохраняя код.
Continue/Step Over/Into/Out/Pause/Stop используют общий button/motion store и отдельные
vector glyphs. Цветовые роли разделяют breakpoint error-dot, pending outline и primary
execution-arrow/highlight. Thread chooser, выбранный frame и variable disclosure
сохраняют общую типографику, мягкие формы, reduced-motion и keyboard focus.
Данные DAP поступают асинхронно; expanded object fields читаются лениво. Навигация
и просмотр не изменяют буфер. Предупреждение о source snapshot не маскируется темой.

## Семантические project actions — этап 9

Usages добавлен пятым сегментом общего dock, не отдельным чужим окном. Ширина
сегментов адаптивна; существующая interruptible spring сохраняет continuity.
Виртуализованные48px строки имеют clipped path/code, полный tooltip, keyboard
focus и keyed Effects hover; reduced motion завершает переход сразу. Breadcrumb
даёт короткие symbol actions с явными shortcut tooltips.
Rename — rounded high surface modal: типографическая иерархия, текстовое имя,
по-файловый preview изменённых строк, error/success diff roles и общие tonal/
filled/ghost кнопки. Apply показывает unsaved tabs, а не скрыто пишет файлы.
Undo rename находится в том же контексте Usages. Ошибки версий/внешних правок
остаются видимыми и не заменяются успешным состоянием темы.

## Debug variable pages and exact transport

Debug preflight checks the exact ADB serial before tool preparation. An explicitly
chosen Wi-Fi transport remains distinct from a USB serial; physical-device regression
also verifies both hardware serial properties before installation. No serial fallback.
Array DAP requests use indexedVariables to bound count to min(100, remaining).
Pages append only at the expected offset within the same session/stop epoch; duplicate
or stale pages never replace existing values. Page errors preserve the loaded prefix
and expose Retry. Continue invalidates page metadata, errors and object references.
The existing Expressive button/spring/reduced-motion component renders Load next/Retry.
JDWP handshake failures retain serial/PID and the real final ART/ADB cause, and race
Attach through an owned error channel; failed attachment drops the adapter/tunnel.

## Native Compose compiler and K2 configuration

ComposeSettings enables a version-matched compiler plugin in each Kotlin module,
with separate per-module metrics/report folders. The compiler toolchain is isolated
from application dependencies/DEX; its plugin hash is checked before publication.
Android module selection imports POM BOM + published module metadata with Android/JVM
runtime priorities, aliases and binary checksum verification. Fixed requirements are
recomputed from the current reachable graph until stable; obsolete requirements do
not force artifacts from a previously selected version. Repeated states/limits fail
without overwriting a previous lock. Android selection has a dedicated lock fingerprint;
legacy POM nearest-first/toolchain locks keep their existing config serialization.
K2 receives the genuine plugin in root kotlinSettings (JPS J{...}), keyed by exact
module/source roots, not app libraries or generated fake ComposableFunction classes.
Native Run smoke drives the actual shared Expressive button using RawInput pointer
hover/press/release, defers validation until the release frame is painted, and waits
for a real matching clean Compose diagnostic snapshot rather than default empty data.

## UX — рабочий сценарий важнее оформления

Пользователь 2026-10-18 отдельно подтвердил приоритет UX. Каждый новый сценарий
проверяется целиком: пользователь понимает действие и результат, проходит его
клавиатурой, видит состояние ожидания/ошибки, исправляет проблему и отменяет операцию
без потери своих правок. Красивый screenshot сам по себе этого не доказывает.

Окно замены теперь содержит подписанные Find и Replace with, параметры рядом и
очевидные Preview/Apply/Cancel. Уже заданный запрос переводит фокус в замену;
пустой — в Find. Enter строит предпросмотр, а Ctrl+Enter применяет только актуальный
непустой review. Подтверждение IME-кандидата не запускает эти действия. Cancel
возвращает фокус инициатору, успешная правка/Undo — редактору.

Действия закреплены вне прокручиваемого содержимого. Высота ограничена реальным
viewport, включая минимум760×560, длинный запрос, старый review и полную диагностику
в Details. Превью показывает сначала различимое имя файла, затем контекст каталога;
полный путь доступен наведением. Устаревший результат остаётся видимым с явной
пометкой и отключённым Apply. No matches и already-this-text объясняют отсутствие
изменений, а пользовательские сообщения не описывают внутренние потоки/снимки памяти.
Уменьшенная анимация и старые ограничения на сохранение/конфликты остаются прежними.

## Git: review before mutation

Git is a separate Material 3 Expressive review surface, opened by **Git changes** in
Project or **Ctrl+Shift+G**. It distinguishes unsaved editor tabs, the working tree
on disk, and staged bytes. No implicit Save, stage-all, push, checkout, discard or
repository initialization occurs. Working/Staged views retain clear selected-file
context; paths show filename first with parent/full-path context. Diff colors are
semantic, and file/diff rows are virtualized. Hooks/signing remain normal Git behavior.

Stage is disabled for dirty affected tabs: explicitly Save, Refresh, review, then
Stage. Unstage never discards working files. Commit requires staged files and a
message, and uses an explicitly labelled button; Enter inside the message does not
submit it. Failure keeps the draft and previous review, disables stale actions and
offers bounded Details + Refresh. A foreign index lock is never deleted. Read jobs
can stop; mutations finish before closing. Escape returns the actual initiating
button/source focus. Footer and message stay outside bounded scrolls at760×560.

## Git daily workflow and external changes

Working tree also lists unsaved-only editor tabs, without pretending their bytes are
already on disk or in Git. **Save selected** is an explicit, narrow asynchronous action;
it saves only dirty tabs associated with that row and returns a fresh reviewed disk
diff. It never stages, commits, discards Undo or saves unrelated tabs. Save conflicts
keep the message/buffers. Stop Save reports actual completed receipts, not “unchanged”.

The file list owns arrows, Home/End/Page, Enter (Open file) and Ctrl+S (Save selected).
F6 switches list/message. File identity uses the raw path, not a shifting row number.
Opening a file reuses dirty parked tabs; following same-frame text is deferred to the
exact accepted source navigation, never another file. Closing supersedes queued opens.
The message remains editable during read jobs, and its Enter never submits a commit.

A lazy3-second idle background refresh runs only while Git is open. Unchanged data
keeps the review; changed data publishes a checked snapshot and selected diff together,
retaining message focus, path and scroll. Errors stop automatic retries and preserve
last-good disabled data. This is polling, not a filesystem event watcher. All read/hash/
diff work stays on the bounded Git worker; both lists virtualize cached/path-indexed rows.

Explicit selected Save/Refresh may move a buffer-only path into the tracked section:
reveal that same path again, rather than leaving the diff disconnected from an invisible
selection. Background observations do not steal the user's scroll or message focus.
Virtualized row geometry is current-frame only. Reduced-motion Git progress is static.

Desktop-only native verification runs one owned Linux session at a time. A test exit,
timeout or exception reaps its exact owned session (including JVM/Git process groups)
using start identities/pidfds, while preserving other apps. This is not Android runtime
coverage and does not terminate deliberately detached sessions.

The private-index copy retains the original Git index mtime. Advancing that timestamp
can incorrectly trust a racily clean equal-size working file and skip required filters.
The deterministic regression compares actual original-index Git rejection against the
private-index operation, requiring failure and unchanged real index/lock cleanup.
Reviewed HEAD also binds its symbolic reference/detached state, not only the commit OID:
an outside switch to another same-OID branch must not redirect Stage or Commit.
At the expanded-error minimum, the file viewport stays60px and reveals the whole44px
selected row; explicit min-scroll sizes also bound diff/details rather than overflowing.

Row double clicks are scoped to the exact path, pointer location and configured time
interval, not egui's global time-only click count. A first row click after another
widget or a different row selects only; a genuine second click opens. Keyboard,
non-list clicks, close/reopen and blur cancel the pair. Resize/expanded diagnostics
reveal a previously visible selection, preserving message focus and intentional scroll-away.
