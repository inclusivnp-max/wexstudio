# WexAndroidStudio

<p align="left">
  <img src="https://img.shields.io/badge/Rust-2024-black?style=for-the-badge&logo=rust&logoColor=white" alt="Rust 2024" />
  <img src="https://img.shields.io/badge/Render-WGPU-blueviolet?style=for-the-badge" alt="WGPU" />
  <img src="https://img.shields.io/badge/Parser-Tree--sitter-green?style=for-the-badge" alt="Tree-sitter" />
  <img src="https://img.shields.io/badge/ADB-Async%20Smart%20Socket-3DDC84?style=for-the-badge&logo=android&logoColor=white" alt="ADB Socket" />
  <img src="https://img.shields.io/badge/LSP-Kotlin%20K2-7F52FF?style=for-the-badge&logo=kotlin&logoColor=white" alt="K2 LSP" />
  <img src="https://img.shields.io/badge/Platform-Linux%20%7C%20Windows-lightgrey?style=for-the-badge" alt="Linux and Windows" />
</p>

Нативный Android workbench на Rust, winit и wgpu. JVM-процессы Gradle и K2
работают отдельно от GPU-интерфейса.

## Рабочие функции

- Ropey + инкрементальный Tree-sitter; Unicode, undo/redo, сохранение с проверкой
  внешних изменений. Рендерятся только видимые строки.
- Прямой async ADB smart socket; устройства, бинарный Logcat, SYNC push и JDWP-forward.
- **Run / Shift+F10**: выбор устройства в верхней панели → сохранение буфера →
  собственная сборка по wex.json → потоковая установка APK → запуск launcher Activity → Logcat этого PID.
  Перед заменой APK старый процесс останавливается, чтобы PackageManager не
  перезапустил прежний task intent поверх нового запуска.
  Сокеты shell-v2 передают APK в PackageManager и возвращают реальный exit code;
  процесс `adb` для установки/запуска/логов не создаётся.
- Native Run получает APK только из успешного receipt собственного build engine,
  после проверки подписи и выравнивания. Старый APK не подставляется после ошибки.
  AGP output listing остаётся частью отдельной совместимости, не пути Run.
- При Run выбранный serial фиксируется до конца операции. Offline/unauthorized
  устройства недоступны; отключение после сборки завершает Run ошибкой без
  переключения на другой телефон. Старые Logcat-события отбрасываются по generation.
- Logcat запускается с replay от времени телефона перед Activity launch: транспортный
  OKAY сам по себе не означает готовности процесса logcat. Retention остаётся 5 MiB;
  после Run сохраняются только записи PID приложения. Stop останавливает Logcat,
  но оставляет приложение запущенным.
- Gradle **Tooling API BuildAction** получает Android **AGP v2 models**, а не
  разбирает `build.gradle` регулярными выражениями. Граф содержит модули,
  варианты, namespace, source/resource/generated roots, boot/compile classpath,
  project dependencies, классы и assemble task.
- Дополнительная tooling-модель получает `R.jar` из объявленных Gradle Task outputs;
  пути внутри `build/intermediates` не угадываются. Bridge запрашивает compile-граф,
  а ошибки зависимостей/синхронизации возвращает как ошибки, не как успешный sync.
- **LMDB** хранит графы отдельно для debug/release. Cache hit обходится без Java
  и Gradle: это проверяется запуском с отсутствующим bridge executable.
- Отпечаток учитывает build/settings scripts, catalogs, properties, wrapper,
  dependency locks, buildSrc/build-logic, Gradle environment и global init/properties.
  Внешние classpath-файлы проверяются по наличию/размеру/mtime. Изменение обычного
  `.kt` не вызывает Gradle sync. Повреждённая запись становится cache miss.
- Подключён настоящий **JetBrains K2/FIR LSP 263.6379.0**, а не имитация сервера.
  Он импортирует экспортированный из LMDB `workspace.json` в отдельном каталоге,
  поэтому для анализа не делает собственный Gradle import.
- В GUI: **Ctrl+Space** — completion, выбор кнопкой в списке; **F12** — definition;
  **Problems** и подчёркивания — живые diagnostics; **Ctrl+S** — save;
  **Ctrl+B** — native Build по wex.json. Кнопки **Connect K2** и **Sync Gradle**
  подключают анализатор или принудительно обновляют модель.
- K2 completion выполняет настоящий `workspace/executeCommand` и применяет
  `workspace/applyEdit` в Rope. Сервер получает `applied=true` только после
  подтверждённого изменения редактора. Устаревшие версии, чужие файлы и
  некорректные диапазоны отклоняются до изменения буфера.
- Latest-buffer watch channel объединяет быстрые изменения; очереди запросов и
  событий ограничены. Анализатор и Gradle не блокируют UI-поток.

## Собственный конфиг и резолвер — без Gradle

`tests/fixtures/android/wex.json` описывает Android SDK levels, applicationId,
модули, source/resource roots, project edges, Maven repositories и pinned dependencies.
`wex resolve` читает его напрямую на Rust; Gradle, Maven и Java не запускаются.

```sh
./target/debug/wex resolve tests/fixtures/android
./target/debug/wex resolve tests/fixtures/android --offline --locked
./scripts/check-native.sh
```

Результат — `wex.lock.json`: граф выбранных версий, POM/JAR/AAR, repository origin,
размер и BLAKE3 каждого файла. Отдельный кэш: `~/.cache/wex/maven` либо `--cache`.
HTTPS transport пока выполняет curl; семантика resolve, POM parsing, cache и lock
реализованы в Rust. Local file repositories поддержаны для воспроизводимых тестов.

Поддержаны transitive compile/runtime POM dependencies, parent POM, properties,
dependencyManagement и BOM imports, optional/test/provided exclusion, wildcard
exclusions, nearest/first-declaration version mediation. Версии фиксированы;
SNAPSHOT/dynamic/ranges кроме exact `[version]` отклоняются. Dependency-affecting
profiles, classifiers и Maven plugin execution требуют расширения. Опубликованные
Android/JVM `.module` variants теперь поддержаны в opt-in Android-режиме; остальные
KMP targets/capabilities — отдельная реализация, см. Native Compose ниже.
`--locked` не меняет файлы, не выполняет сеть и проверяет каждый cached byte;
изменение конфига, пропавший файл или несовпадение hash завершает операцию ошибкой.

На реальных зависимостях Android fixture получены **20 artifacts / 43 locked files**,
включая `androidx.core:core:1.13.0` и pinned Kotlin stdlib 2.4.0. Повтор с пустым PATH
и `--offline --locked` подтверждает независимость от Gradle/Java/network helpers.
Собственный APK-конвейер теперь подключён к Build и Run:

1. Locked dependency graph; JAR/AAR extraction с проверкой путей/размера.
2. Manifest merge: namespace/applicationId, `${applicationId}`, полные component
   names, SDK/version/debug settings, library permissions/providers/metadata.
3. AAPT2 compile/link: ресурсы AAR и локальных модулей, generated R/BuildConfig,
   общие конечные resource IDs для app/library packages.
4. Собственная Android java.base system image через javac/jar/jmod/jlink из SDK
   `core-for-system-modules.jar`; Java компилируется против Android stubs.
5. Настоящий Kotlin K2 CLI 2.4.0 + javac, topological module order и mixed Java/Kotlin.
6. D8, JAR resources/assets/JNI packaging, zipalign (16 KiB pages), apksigner debug
   signing и обязательные signature/alignment verification.
7. Атомарный `current.json` только после успеха; ошибки не заменяют прежний artifact.
   Отпечаток исходников/ресурсов/конфига проверяется до и после сборки.
8. Native ProjectGraph импортируется K2 LSP через workspace.json без Gradle sync.

```sh
./target/debug/wex prepare-tools tests/fixtures/native-android
./target/debug/wex build tests/fixtures/native-android --module :app --offline
./target/debug/wex run tests/fixtures/native-android --serial SERIAL --offline --require-marker WEX_NATIVE_OK
```

Compiler JARs загружаются собственным Maven resolver в отдельный toolchain lock;
Gradle/Maven процессы не используются. SDK Build Tools 36.0.0 и JDK21 должны быть
установлены; builder их не скачивает. `ANDROID_HOME`/`ANDROID_SDK_ROOT`, `JAVA_HOME`
и `WEX_MAVEN_CACHE` задают размещение инструментов/кэша.

Native fixture содержит только wex.json/lock и Android sources/resources/assets,
без Gradle-файлов. В нём Java-класс вызывается из Kotlin, межмодульный Kotlin-код
использует runtime stdlib, а shared resource и asset проверяются внутри приложения.

По указанию пользователя новые Gradle distributions не скачиваются. Старый bridge
получает только проверенный локально распакованный distribution проекта через
`useInstallation`; Отдельная команда `gradle-build` проверяет его наличие до запуска wrapper. Новый native
build path развивается на `wex.json`/`wex.lock.json`, не на Gradle scripts.

## Быстрый запуск в этом checkout

```sh
cd /home/zapahtravki/wexstudio/wexandroidstudio
python3 scripts/install-kotlin-lsp.py
cargo build --release --features desktop --bins --locked
./target/release/wex resolve tests/fixtures/native-android
./target/release/wex prepare-tools tests/fixtures/native-android
./target/release/wex build tests/fixtures/native-android --offline
env -u WAYLAND_DISPLAY ./target/release/wex-ide tests/fixtures/native-android
```

Native SDK задаётся через `ANDROID_HOME`/`ANDROID_SDK_ROOT` или `$HOME/Android/Sdk`.
`local.properties` используется только старой Gradle-совместимостью.
Для проверочного проекта `scripts/check-android.sh` создаёт `local.properties`
по пути SDK. Для своего проекта замените `tests/fixtures/android` его каталогом.

При открытии проекта с wex.json GUI отдельно подготавливает Android-модель для K2:
собственный resolver, AAPT2 и Javac генерируют настоящие R/BuildConfig до первой
сборки APK. Kotlin/Java исходники приложения при этом не компилируются; синтаксические
ошибки не мешают открыть проект и получить диагностику. Модель, classpath и source/
generated roots кэшируются в LMDB и передаются через JSON workspace. Gradle не импортируется. Проекты без
wex.json могут использовать отдельный legacy bridge для анализа, но native Run
требует wex.json и никогда не переключается на Gradle после ошибки.

`prepare-bridge.py` использует установленный Gradle 9.6.0 из wrapper cache или
`WEX_GRADLE_HOME`; компилирует небольшую Java-прослойку через `javac`. AGP model
JAR берётся из Gradle cache или официального Google Maven. Kotlin installer
скачивает неизменённый официальный архив и проверяет закреплённый SHA-256.

Настройки:

- `WEX_KOTLIN_LSP` — абсолютный путь к другому launcher, совместимому с указанной матрицей.
- `WEX_GRADLE_BRIDGE` — путь к `scripts/gradle-bridge.sh` в переносимом размещении.
- `WEX_GRADLE_HOME` — Gradle 9.6.0 distribution для compilation/runtime bridge.
- `JAVA_HOME` — JDK для Kotlin workspace; на Linux по умолчанию разрешается `/usr/bin/java`.
- `WEX_K2_SYSTEM_DIR` — отдельный каталог K2 для независимых окон/проверок;
  по умолчанию `PROJECT/.wex/k2-system`.
- `WEX_LSP_TRACE` — опциональный путь к локальному JSON-RPC trace. Он содержит
  текст открытого буфера; по умолчанию trace выключен.

Проверенная native матрица: Linux x86_64/X11, Rust 1.99, JDK 21, SDK Build Tools 36.0.0,
Kotlin CLI 2.4.0, K2 LSP 263.6379.0. Legacy-мост проверялся на Gradle 9.6.0,
AGP 9.2.1, Kotlin Gradle plugin 2.4.0, Android SDK 36, K2 LSP 263.6379.0.
Wayland остаётся отдельным пунктом проверки; подтверждённый запуск здесь — X11.

## CLI

```sh
./target/release/wex sync /absolute/android/project --variant debug
./target/release/wex sync /absolute/android/project --variant release --offline
./target/release/wex sync /absolute/android/project --force
./target/release/wex k2-check tests/fixtures/android
./target/release/wex build tests/fixtures/native-android --module :app --offline
./target/release/wex devices
./target/release/wex device-info --serial SERIAL
./target/release/wex install --serial SERIAL /absolute/app.apk
./target/release/wex run /absolute/android/project --serial SERIAL --module :app --variant debug --log-seconds 5
./target/release/wex logcat --serial SERIAL --regex 'Activity|Wex' --min-priority 4
./target/release/wex push --serial SERIAL sample.apk /data/local/tmp/sample.apk
./target/release/wex forward-jdwp --serial SERIAL --port 8700 --pid 1234
```

`k2-check` — конкретный сквозной семантический сценарий для checked-in Android app,
а не общий валидатор произвольного проекта. Он меняет только LSP-буфер, не исходник
на диске. Устройства требуют уже запущенного ADB-сервера; клиент не запускает
`adb` на каждую операцию. Push переносит APK; Install устанавливает; Run выполняет
весь цикл и выводит JSON receipt с хэшем APK, remote exit codes, component, PID и логами.
CLI допускает выбор module; GUI использует `build.application_module` из wex.json.
Native build поддерживает debug с постоянным Android debug key и release с
явной пользовательской подписью (см. раздел ниже). Arbitrary compiler plugins,
flavors/source-set overrides и полная Maven/KMP variant/capability семантика
требуют расширений. Native Compose compiler уже
подключён, см. соответствующий раздел ниже. При неподдержанной конфигурации сборка завершается ошибкой.
`--require-marker` предназначен для тестов: требует marker с новым runId в логах PID.
Обычные Android-приложения не обязаны писать Wex-маркеры.

Совместимый Gradle-путь выбирает версию **wrapper проекта**, только если она уже локально установлена.
Проверочный проект закрепляет Gradle 9.6.0 с SHA-256; автоматически последняя версия
не подставляется. Клиент bridge отдельно закреплён на Tooling API 9.6.0 и AGP models
9.2.1. Проверенная матрица не означает совместимость со всеми Gradle/AGP.

## Проверки

```sh
./scripts/check.sh
./scripts/check-android.sh
# Явно выбранный реальный телефон: устанавливает/запускает только fixture app.
WEX_BIN=./target/release/wex WEX_IDE=./target/release/wex-ide ./scripts/check-phone.sh SERIAL
```

Обычная проверка: fmt, Rust unit/GUI-input tests, clippy `-D warnings`,
black-box CLI с изолированными protocol/wrapper fixtures.

Android-проверка использует настоящий двухмодульный **native** Android-проект:
Kotlin/Java, AAR dependencies, module resources/assets, manifest merge, DEX,
signature/alignment, повторяемость, failed build/tamper, настоящий K2 и GPU smoke.
Файлов Gradle в native fixture нет; Gradle/Maven traps подтверждают отсутствие запусков.
Проверка физического телефона — отдельный явный запуск с serial; результаты
не подменяются успехом локальной сборки.

## Текущие границы реализации

- Открытие, синтаксический разбор, Save, поиск, массовая замена и обработка
  внешних изменений выполняют тяжёлую работу в фоне; детали и проверки ниже.
  Soft-wrap, multi-cursor, bidi-layout и экранный reader требуют следующих этапов.
- URI библиотечного класса из JAR показывается как destination; decompiler-view
  ещё не добавлен. Переходы в Kotlin и generated Java source открывают файл.
- Произвольные compiler plugins, composite/included builds за пределами root,
  тестовые source sets и остальные версии AGP требуют расширения моделей и тестов.
  Native Compose settings и соответствующий Kotlin LSP compiler plugin проверены.
- Install/Run использует подписанный single/universal APK с launcher Activity; split APK sets,
  bundle/APKS, кастомные run configurations и multi-user PID selection ещё требуют
  расширения. При неоднозначном PID операция возвращает ошибку.
- Реальные XML/Compose Layoutlib preview и DAP Debug реализованы и описаны ниже;
  полный паритет возможностей Android Studio ещё требует дальнейшей работы.
- Logcat retention budget 5 MiB не означает RSS всего приложения. RSS, холодный
  старт, миллион строк/сек и размер полного дистрибутива ещё требуют измерений.
- Индекс K2 хранится его собственным движком; LMDB здесь используется для Android-графа.

Выбран путь wgpu+winit через eframe; форка Zed нет.
Графический клиент остаётся Rust-приложением, JVM работает только в backends.

## Первичные источники

- [Gradle Tooling API](https://docs.gradle.org/current/userguide/tooling_api.html)
- [Android AGP v2 models](https://android.googlesource.com/platform/tools/base/+/refs/heads/mirror-goog-studio-main/build-system/builder-model/src/main/java/com/android/builder/model/v2/models/)
- [Kotlin LSP releases](https://github.com/Kotlin/kotlin-lsp/blob/main/RELEASES.md)
- [Kotlin JSON workspace importer/schema](https://github.com/Kotlin/kotlin-lsp/tree/main/workspace-import/src/com/jetbrains/ls/imports/json)
- [Kotlin completion commands](https://github.com/Kotlin/kotlin-lsp/blob/main/features-impl/common/src/com/jetbrains/ls/api/features/impl/common/completion/LSCompletionProviderHelper.kt)
- [LSP specification](https://microsoft.github.io/language-server-protocol/specifications/lsp/3.17/specification/)
- [Android logger_entry](https://android.googlesource.com/platform/system/logging/+/refs/heads/main/liblog/include/log/log_read.h)

## Дополнительные локальные проекты

- `/home/zapahtravki/Tools/clazzpather-for-wex`: по README содержит class/DEX/smali
  readers/writers и converters; потенциальная основа отдельного bytecode inspector.
- `/home/zapahtravki/Tools/kt210426`: по README содержит Kotlin interpreter и
  class/DEX backends; кандидат для scratch/REPL. Это отдельный движок, не замена K2.

В текущую сборку эти проекты не включены; их исходники не менялись.

Первичные источники Run:
[ADB PackageManager streaming install](https://android.googlesource.com/platform/packages/modules/adb/+/refs/heads/main/client/adb_install.cpp),
[Activity Manager](https://developer.android.com/tools/adb#am),
[Logcat timestamp replay](https://android.googlesource.com/platform/system/logging/+/bc3bc623dc0919334ab13956aae77ce5553a5a90/logcat/logcat.cpp).

Первичные источники собственного resolve:
[Maven dependency semantics](https://maven.apache.org/guides/introduction/introduction-to-dependency-mechanism.html),
[Maven POM model](https://maven.apache.org/pom.html).

## Native проверки и дизайн

`./scripts/check-native-build.sh` проверяет настоящий SDK/compiler pipeline,
содержимое DEX/APK/manifest/assets, подпись v2/v3, выравнивание, byte-identical repeat,
ошибочный Java input и tampered APK. Gradle/Maven executables заменяются traps;
проект не содержит Gradle-файлов. Затем проверяются настоящий K2 и GPU-окно.
`./scripts/check-android.sh` теперь вызывает этот native сценарий.

`./scripts/check-phone.sh SERIAL` запускает native fixture через CLI и GUI Run;
требует свежих onCreate/onResume и WEX_NATIVE_OK с Java/resource/asset proof.


Дизайн самой IDE — **Material 3 Expressive**. Основа и дальнейшие правила:
[DESIGN.md](DESIGN.md). Уже добавлены semantic surface/primary roles, мягкие формы,
типографика, общие панели/вкладки/chooser/dialogs, прерываемые spring-переходы и reduced motion. Компоненты описаны ниже.

SDK/compiler reference:
[AAPT2](https://developer.android.com/tools/aapt2),
[Kotlin compiler](https://kotlinlang.org/docs/compiler-reference.html),
[D8](https://developer.android.com/tools/d8),
[Signing](https://developer.android.com/tools/apksigner),
[Material Expressive motion](https://m3.material.io/styles/motion/overview/how-it-works).

### Expressive workspace UI

IDE uses a coherent Material 3 Expressive component system across its app bar,
navigation rail, project tree, document tabs, device chooser, output dock and
modal dialogs. Dirty tabs preserve Rope buffers and undo history; Build/Run save
all open dirty documents. Preferences contains K2/model actions and Reduce motion
(`WEX_REDUCED_MOTION=1` also works). Spatial springs retarget without resetting
position or velocity. The compact toolbar keeps device, Save, Build and Run
available from 760 logical pixels. See [DESIGN.md](DESIGN.md).

Native-window UI regression (no deployment), including PNG and motion-report checks:

```sh
./scripts/check-ui.sh tests/fixtures/native-android .verification/expressive-live
```

This exercises pointer/key input in the actual GPU window and writes screenshots
plus `live-window.json`. Normal use omits `--ui-smoke`.


### Редактор — этап 6

- Выделение drag/Shift-click, двойной клик по слову, тройной клик или gutter по строке;
  автопрокрутка выделения за границей viewport. Shift+стрелки/Home/End/PageUp/PageDown,
  Ctrl+стрелки по словам, Ctrl+Home/End по документу, Ctrl+A.
- Системные Copy/Cut/Paste (Ctrl+C/X/V и контекстное меню), Ctrl+Z/Y/Shift+Z,
  Tab/Shift+Tab для отступов, Enter с сохранением отступа. Undo/redo восстанавливает
  выделение; вкладки сохраняют также scroll и горизонтальную позицию.
- Ctrl+F/H: поиск/замена; F3/Shift+F3 или Enter/Shift+Enter в поле поиска;
  Escape возвращает фокус к коду. Case, Unicode whole-word, regex, область исходного
  выделения. Regex replacement: `$1`, `${name}`, `$$`, `\n`, `\t`; literal mode
  не интерпретирует текст замены. Replace All — одна отмена. До 50,000 результатов;
  усечённый список не допускает частичной Replace All под видом полной.
- Перемещение/удаление учитывает grapheme clusters (combining marks, ZWJ emoji, CRLF).
  IME preedit отображается временно, не меняет файл/undo/K2 до commit; commit — одна
  операция. Escape/потеря фокуса отменяет композицию, поздний commit отбрасывается.
  Платформе передаётся положение caret для candidates. Установленный Noto CJK служит
  необязательным последним fallback, без загрузки или включения шрифта в executable.
- Watcher проверяет открытые/parked файлы в фоне: quiet250ms/max1500ms debounce,
  metadata fast path + content scrub каждые2s; дерево проекта обновляется раз в1s.
  Чистый буфер автоматически перечитывается с сохранением line/column; изменённый
  буфер/активный preedit сохраняется и получает inline conflict. Reload требует
  подтверждения потери правок; Keep mine принимает наблюдённую версию без записи.
  Save повторно сверяет hash/отсутствие перед atomic rename. Удалённый файл сохраняет
  буфер; Recreate on Save явно разрешает его восстановление. Ошибка чтения не
  разрешает перезаписать файл. Build/Run останавливается на неразрешённом конфликте.
- UI использует общий Expressive component/motion system: анимированная find card,
  tonal flags, inline review card и modal dialog. 10,000 строк остаются виртуализованы.

```sh
./scripts/check-editor.sh tests/fixtures/native-android .verification/editor-live
```

Проверка создаёт только собственные временные Kotlin-файлы, подаёт input в настоящее
winit/wgpu окно, выполняет native OS clipboard round-trip и меняет файлы отдельным
процессом. Она проверяет IME-события/preedit/commit/cancel и candidate rectangle;
переключение конкретного системного IME-демона не автоматизировано этим сценарием.
Результат: 10 PNG и `editor-window.json`, неизменённые исходники проекта, удалённые
scratch-файлы. Для проверки используется отдельный `WEX_K2_SYSTEM_DIR`.

Из WEXdesktop взяты идеи SearchModel (selection scope, flags, bounded matches,
одна undo-транзакция) и DirWatcher (quiet/max debounce). Реализация собственная,
на Rust/Rope/egui; `/home/zapahtravki/IdeaProjects/WEXdesktop` не изменялся и не собирался.

### Android Debug — этап 7

Debug (F5) выполняет **wex.json → native APK → установка → `am start -D` → JDWP**
на явно выбранном устройстве. Rust работает с ADB smart socket напрямую;
локальный одноразовый loopback tunnel подключает JDK JDI к `jdwp:PID`. Системные
ADB-forward rules не создаются. Стандартный **Microsoft Java Debug Server 0.53.1**
работает отдельным процессом и общается с Rust по DAP. JVM не входит во frontend.
JAR-файлы закреплены URL/SHA-256 в `debugger/dependencies.json`; подготовка выполняется
`python3 scripts/prepare-debugger.py` (повторно — `--offline`), без Maven/Gradle/JDT LS.
JDK 21 выбирается через `WEX_DEBUG_JAVA_HOME` (по умолчанию системный OpenJDK21).

- Клик в крайней левой части gutter или F9 переключает line breakpoint. Числовая
  область gutter по-прежнему выделяет строку. Точки сохраняются в `.wex/breakpoints.json`.
  Полый маркер означает pending/unverified, заполненный — verified/локальную точку.
- F5 — Debug/Continue, F6 — Pause, F10 — Step Over, F11 — Step Into,
  Shift+F11 — Step Out, Shift+F5 — Stop. Shift+F10 остаётся обычным Run.
- Панель Debug содержит thread chooser, call stack, выбор кадра, scopes и ленивый
  просмотр полей объектов/элементов массивов. Следующая исполняемая строка отмечается
  отдельно от красной точки; выбор доступного source-frame открывает его в редакторе.
- Scope/variables replies привязаны к session/stop epoch и selected frame/thread;
  поздние данные после Continue, другого кадра или новой сессии отбрасываются.
- Build/Run/смена устройства блокируются на время Debug; сессия хранит собственные
  serial/PID. Stop отключает адаптер, завершает только выбранное приложение на том же
  устройстве и проверяет исчезновение PID. Потеря ART/адаптера снимает paused-state.
- Javac получает `-g`, D8 — `--debug`. SourceFile/LineNumberTable текущей опубликованной
  сборки связывают Java/Kotlin source с классами, включая nested/multiple classes.
- Debug использует сохранённый снимок исходников. Изменённый source не выдаётся за
  исполняющийся APK: появляется предупреждение, скрывается старый execution highlight,
  live breakpoint edits для несовпадающего снимка блокируются. После изменения строк
  требуется перезапуск и проверка положения сохранённых точек. Hot reload, expression
  evaluation, conditional/log breakpoints и Kotlin coroutine/inline/SMAP-aware stepping
  — отдельные расширения; соответствующие действия в UI не заявляются.

Проверка **устанавливает/запускает** fixture APK на указанном физическом телефоне:

```sh
./scripts/check-debug.sh 47101FDAS0028Q tests/fixtures/native-android .verification/debug-phone
```

Сценарий проходит CLI и native GPU IDE: `seed=7`, `doubled=14`, вход в nested function,
шаг с `adjusted=17`, выход с return value17, caller/object variables, удаление точки,
Continue → свежий PID-bound Logcat, Pause и Stop/PID gone. Сохраняются DAP traces,
JSON receipts и 13 PNG. Массив из 250 элементов загружается страницами
100/100/50, последнее значение — 747. Native-window input подаётся через контролируемый eframe
RawInput hook; test-only focus isolation отделяет этот поток от Niri/Xwayland FocusOut.
Это реальное winit/wgpu окно и физический ART, не mock debugger. Оригинальные source
и breakpoint preferences проверяются/восстанавливаются после теста.

Первичные источники: [Java Debug Server](https://github.com/microsoft/java-debug),
[DAP](https://microsoft.github.io/debug-adapter-protocol/),
[JDK JDI](https://docs.oracle.com/en/java/javase/21/docs/api/jdk.jdi/module-summary.html),
[Android debuggers](https://source.android.com/docs/core/tests/debug/gdb).

### Надёжность сессии Debug

Быстрые переключения точек отправляют не пересекающиеся `setBreakpoints` для
каждого файла: промежуточные наборы объединяются, последнее удаление тоже
доставляется. Устаревшие ответы не восстанавливают маркеры и не разблокируют
кнопки нового шага. Завершение адаптера инвалидирует старую сессию. Pause ждёт
список реальных потоков и изначально выбирает main; ошибка Stop отображается
отдельно от подтверждённого исчезновения PID.

Локальная интеграционная проверка **настоящего** Java Debug Server/JDI (не Android):

```sh
python3 tests/debugger_jvm_e2e.py .verification/jvm-debug.json
```

Она компилирует временный Java-проект, проверяет точки, стек, переменные и шаги
7 → 14 → 17, удаление точки, Continue и завершение VM. Нужны подготовленные
pinned debugger JAR и JDK21. Проверку физического телефона по-прежнему выполняет
`check-debug.sh` с явно переданным serial; локальный JVM receipt её не заменяет.

## Семантический поиск использований и Rename — K2

- **Shift+F12** / действие в breadcrumb — поиск использований символа, включая
  declaration. Панель **Usages** группирует пути/строки и открывает точный range.
- **Shift+F6** / Rename — настоящий `prepareRename` → `rename`, а не поиск текста.
  Preview показывает изменяемые строки по всем файлам; Apply меняет буферы,
  включая ещё не открытые файлы. Диск изменяется только обычным Save/Build/Run.
- **Undo rename** в Usages отменяет последнюю project transaction целиком.
  После независимых правок/закрытия изменённой вкладки команда не делает частичный
  откат; остаётся обычная история Undo отдельных документов. Save между Rename
  и Undo сохраняет правильный disk baseline.
- Все открытые Kotlin/Java tabs, включая parked/dirty, остаются открыты в K2.
  Собственный UI snapshot clock отделён от последовательных wire revisions;
  версии серверных edits проверяются до перевода в версии UI. Project actions
  ждут подтверждения успешного импорта, чтобы не переименовывать неполный граф.
- План проверяет UTF-16 ranges, overlap, версии, membership исходников и disk
  hashes. Parsing/preparation выполняются фоново; подготовленные документы
  заменяются одной UI transaction только после повторной проверки буферов.
  Resource operations (create/delete/rename файла) требуют отдельного review
  и здесь отклоняются, а не исполняются как текстовые правки.

```sh
./scripts/check-refactor.sh tests/fixtures/native-android .verification/refactor-live
python3 tests/refactor_native_e2e.py .verification/refactor-live/refactor-window.json .verification/refactor-android emulator-5580
./scripts/check-debug.sh emulator-5580 .verification/refactor-android/renamed-android .verification/mtvm-debug virtual
```

Первый сценарий использует настоящий K2 в GPU окне: четыре usages в двух модулях,
включая unsaved parked caller, prepare/preview/apply, повторный анализ без Save
и общий Undo. Комментарий `greeting` и соседний `greetingCount` остаются прежними;
K2 возвращает16 мелких TextEdits для четырёх символов. Второй сценарий собирает
именно возвращённые K2 исходники в отдельной копии и проверяет APK/запуск/Logcat.
Третий проверяет реальный ART/DAP debugger в явно выбранной VM. `virtual` — явный
режим: без него check-debug продолжает требовать физическое устройство.

Raw Logcat captures проверяются тем же streaming decoder без телефона:

```sh
./target/debug/wex logcat-replay /absolute/path/to/logcat.bin
```

Результат содержит bytes/entries/truncated_messages/elapsed_micros. Полный Android
payload4068 может потерять завершающий NUL при ограничении liblog; decoder
помечает `truncated`, сохраняет последний байт и следующий frame. Короткий
неоконченный payload по-прежнему считается malformed.

### Debug: точный transport и большие массивы

- Debug проверяет точный ADB serial **до** подготовки адаптера и сборки. Если USB
  отсутствует, IDE не подменяет его Wi-Fi serial или эмулятором. Ошибка содержит
  выбранный serial и состояние; device commands до успешного preflight не идут.
- Для явной проверки того же физического телефона через Wi-Fi задайте полный ADB
  transport и ожидаемый аппаратный serial пятым аргументом. Сценарий проверит оба
  `ro.serialno`/`ro.boot.serialno` и physical-vs-emulator до установки:

```sh
./scripts/check-debug.sh adb-47101FDAS0028Q-syuJun._adb-tls-connect._tcp \
  tests/fixtures/native-android .verification/debug-phone physical 47101FDAS0028Q
```

- Массивы раскрываются по 100 элементов. **Load next 100** использует продолжение
  по DAP `start`/`count`; последний запрос ограничен реальным `indexedVariables`,
  а не выходит за границы массива. Загруженные страницы остаются доступны до
  Continue/смены stop epoch. Ошибка страницы сохраняет данные и показывает Retry.
- ART JDWP handshake ограничен по времени, повторяет раннее закрытие и сообщает
  реальный последний сбой вместе с serial/PID. Ошибка туннеля поступает в результат
  Attach, а не остаётся только скрытым сообщением фонового процесса.
- Кнопки страниц используют общий Material 3 Expressive компонент со spring/reduced
  motion. Сессия/epoch, пустой массив, повторный ответ и границы страниц проверяются
  отдельно; сценарий телефона проверяет реальные DAP данные и native GPU окно.

## Native Compose без Gradle

`build.compose` включает **плагин компилятора той же версии, что Kotlin** для каждого
Kotlin-модуля. Это реальное преобразование `@Composable`, а не только добавление
Compose-библиотек в classpath. Плагин хранится отдельно в compiler toolchain и не
включается в APK/DEX или app classpath. `BuildReport.compose` фиксирует версию, путь,
BLAKE3, включённые параметры и реально скомпилированные модули.

```json
{
  "android_variants": true,
  "platforms": ["androidx.compose:compose-bom:2025.10.01"],
  "constraints": ["androidx.compose.material3:material3-android:1.4.0-alpha18"],
  "build": {
    "kotlin_version": "2.4.0",
    "compose": {
      "source_information": true,
      "reports": true,
      "strong_skipping": true
    }
  }
}
```

Это фрагмент конфигурации; полный двухмодульный проект — `tests/fixtures/compose-android`.
Он использует MaterialExpressiveTheme, выразительные Button shapes, состояние счётчика
и общий Kotlin/Java-модуль. Зафиксированная alpha-версия Material3 выбрана потому, что
в ней публичен используемый Expressive API; это fixture, не автоматический выбор
версии для чужих проектов. Compile SDK36/build-tools36 и JDK21 готовятся отдельно.

### Resolver для Android/KMP

- По умолчанию прежний POM nearest/first режим сохранён. `android_variants: true`
  дополнительно читает опубликованные `.module` JSON, выбирает Android/JVM runtime
  и canonical available-at aliases; dependency-only bridges не получают ложный JAR.
  Gradle runtime, дистрибутив и Maven executable для этого не запускаются.
- Android-режим выбирает максимальные **фиксированные** требования текущего reachable
  графа. При изменении выбранной версии граф пересчитывается; старые edges и файлы
  удаляются из итогового lock. Циклы выбора/лимиты явно останавливают resolve и не
  заменяют предыдущий lock. Это собственный ограниченный resolver, не реализация
  всего Gradle variant/capability/rich-version алгоритма.
- `platforms` импортирует Maven POM BOM; первый BOM выигрывает. `constraints` задаёт
  enforced exact или `group:*:version` pins; explicit exact pin выигрывает у group
  pin/BOM. Ни constraints, ни BOM сами по себе не добавляют недостижимые библиотеки.
- `.module`, POM и binaries входят в BLAKE3 lock. Опубликованные binary size/SHA-256
  проверяются дополнительно. Android selection algorithm имеет отдельный fingerprint
  в `mediation`; устаревший lock требует явного повторного resolve.
- Dynamic/range versions, classified binaries и arbitrary requested capabilities
  требуют отдельной реализации; resolver не подменяет их случайной библиотекой.

### Compose в K2 и Run

Native K2 workspace экспортирует root `kotlinSettings` для **каждого** модуля:
module/sourceRoots, JVM target и настоящий проверенный plugin JAR через JPS `J{...}`
compilerArguments. Обычные JSON module facets не заменяют этот раздел. Подставные
ComposableFunction-классы и фильтрация compiler diagnostics не используются.
`wex k2-check` выбирает единственный MainActivity.kt или explicit root-relative
`--file`; для Compose fixture проверяет completion, Kotlin/Java definitions,
реальные type/Compose-call ошибки и восстановление буфера без записи на диск.

```sh
./target/debug/wex resolve tests/fixtures/compose-android
./target/debug/wex prepare-tools tests/fixtures/compose-android
./scripts/check-compose.sh tests/fixtures/compose-android .verification/compose emulator-5580
```

Последний сценарий ожидает уже запущенную mtvm с явно переданным serial. Он проверяет
native compile/lowering, metrics/flags, повторяемую подпись, private plugin tamper,
настоящий K2, установку/первый Compose frame, Android input tap и recomposition0→1,
а затем реальный pointer click кнопки Run в native IDE и чистый K2 diagnostic snapshot.
Приложение останавливается после проверки. Два снимка устройства и снимок IDE —
runtime evidence, не headless Layoutlib preview. Сам Layoutlib/Compose preview ещё
остаётся следующим самостоятельным этапом полной IDE.

Compiler-settings schema сверена с bundled Kotlin LSP263.6379.0 и его
[официальным обсуждением](https://github.com/Kotlin/kotlin-lsp/issues/106).


### Полный стек Android Debug

Gutter/F9 переключает breakpoint; Debug/F5 сохраняет буферы, собирает APK по
`wex.json`, устанавливает его на выбранный serial и подключает настоящий Java Debug
Server через прямой ADB/JDWP. F10 — шаг с обходом, F11 — вход, Shift+F11 — выход,
F6 — пауза, Shift+F5 — остановка с проверкой исчезновения PID. Панель Debug содержит
потоки, выбор кадра стека, locals/объекты и постраничные массивы. DAP expression
watch/evaluation и coroutine/inline-aware mapping остаются отдельными задачами.

Стек теперь запрашивается целиком: pinned Java Debug Server сообщает
`supportsDelayedStackTraceLoading=false`, поэтому `levels: 50` скрывал глубокие
кадры. UI рисует только видимые строки, показывает число кадров, Loading/Error/Retry
и сохраняет последний успешный snapshot при ошибке. Неполный/некорректный ответ,
дубликаты ID и превышение 10 000 кадров получают явную ошибку вместо тихого усечения.
Session/stop/thread guards сохраняются при повторном запросе и смене потока.

```sh
# Физический Android: только точный USB transport, без автоматической подмены Wi-Fi.
bash scripts/check-debug.sh 47101FDAS0028Q tests/fixtures/native-android .verification/debug-usb physical
# Независимая host-регрессия, не подтверждение отладки телефона:
bash scripts/check-debug-host.sh .verification/debug-host
```

Host-проверка использует настоящий JDK21/JDWP и pinned DAP adapter: 73 кадра,
переменная `depth=60` у глубокого caller, breakpoint 7/14, шаги до 17, массив
100/100/50, завершение процесса и реальное окно Material 3 Expressive. Запуски
Gradle/Maven/ADB executable/emulator блокируются тестовыми traps. Телефон проверяет
отдельный сценарий: exact serial preflight → native build → install → JDWP/DAP →
13 GPU screenshots → свежий PID-bound Logcat → Stop. Отсутствующий USB serial
останавливает этот сценарий до сборки/установки и не считается успешной проверкой.

## Headless Compose preview

Соседняя панель **Preview** рендерит настоящий Compose через Layoutlib в отдельном
фоновом JVM-процессе. Изображение приходит от `com.android.layoutlib.bridge.Bridge`,
не от телефона/эмулятора и не из макета. Rust/wgpu frontend остаётся нативным.

Предварительно нужны локальный Android SDK36/JDK21 и Layoutlib из установленной
Android Studio (проверено с build261.26222.65.2613.16025427 в `/opt/android-studio`).
`prepare-preview` ничего не скачивает из Studio/Gradle: компилирует собственный
adapter, фиксирует SHA256 runtime/renderer files и готовит ui-tooling через наш
offline Maven resolver. Другое место установки задаёт `WEX_LAYOUTLIB_STUDIO`;
JDK задаёт `WEX_PREVIEW_JAVA_HOME`. Зависимости приложения должны быть уже resolved
и locked; недостающие tooling artifacts сначала готовятся обычным собственным
resolver, а не Gradle. `WEX_PREVIEW_TOOLS` позволяет явно выбрать tools manifest.

```sh
./target/debug/wex prepare-preview tests/fixtures/compose-android --offline
./target/debug/wex preview tests/fixtures/compose-android \
  --entry Counter --output /tmp/wex-preview.png
./scripts/check-preview.sh tests/fixtures/compose-android .verification/headless-preview
```

Для явных сцен рядом с `wex.json` опционально задаётся `wex.preview.json`, чтобы preview metadata не
меняла dependency lock приложения:

```json
{
  "schema": 1,
  "entries": [{
    "name": "Counter",
    "composable": "dev.wex.compose.MainActivityKt.WexPreview",
    "width_dp": 360,
    "height_dp": 640,
    "dark": false,
    "font_scale": 1.0
  }]
}
```

Эти ручные entries сохраняют совместимость; теперь они дополняются автоматическим
каталогом compiler-emitted `@Preview`/multipreview (см. ниже). Функция fixture использует
MaterialExpressiveTheme, CounterCard из другого Kotlin/Java-модуля и `R.string` из
скомпилированных ресурсов. Пример темы реагирует на `isSystemInDarkTheme()`; чужая
composable сама определяет, использует ли системный night mode.

- Сборка snapshots использует тот же реальный Kotlin Compose plugin/AAPT2/Javac,
  но останавливается на JVM-классах и compiled resources. D8, APK signature,
  install и публикация `current.json` в **preview pipeline** отсутствуют.
- Ui-tooling разрешается единым графом с project dependencies той же версии
  Compose UI; не попадает в root app lock/classpath/DEX. Фактические compile steps,
  plugin hashes, resource hash, source/buffer versions и renderer receipt сохраняются.
- Kotlin/Java/XML dirty buffers копируются в изолированный temp project; оригинальные
  исходники не сохраняются. Все inputs/их inventory повторно проверяются перед
  публикацией. Границы:256MiB snapshot/20k files,8MiB per dirty buffer/64MiB overlays.
- **Refresh**, **Live** (debounce800ms), **Dark**, **Cancel** и закрытие панели используют
  общий M3 Expressive UI. Процессы ограничены по времени и завершаются при отмене;
  worker snapshot живёт до окончания фонового копирования. Decode PNG выполняется
  вне UI-потока; старые generations не заменяют новый результат.
- Ошибка компиляции/renderer оставляет последнее удачное изображение со stale label.
  Layoutlib SUCCESS сам по себе недостаточен: проверяются errors, настоящий
  ComposeViewAdapter, непустые Compose view infos, размеры и PNG. Broken-view
  placeholder не публикуется как успешная сцена.
- Runtime и собственные adapter classes проверяются SHA256; повторные проверки
  используют только неизменённые file identities/mtime/size (на Unix также ctime).
  Изменённый private renderer отклоняется до сборки, не меняя последнее изображение.
- Live отслеживает открытые/parked buffers и их external reload. Для изменений
  закрытых файлов/новых файлов после завершения render пока нужен Refresh;
  изменения во время render обнаруживает полная повторная проверка inputs.

Текущие границы: discovery требует успешной snapshot compilation, нет interactive
Compose input/animation clock; compilation пока full-project, не incremental.
Compiled strings/colors и текущий fixture проверены; arbitrary binary drawable XML,
custom render extensions и все Compose/Studio версии требуют отдельных проверок.
Это настоящий headless preview, но не утверждение о полной совместимости Studio.

## Debug / Release APK и пользовательская подпись

В верхней строке проекта нажмите название/подзаголовок: **APK build variant** выбирает
`debug` или `release`. Выбор сохраняется в `.wex/apk-variant.json`; Build, Run и K2
используют один выбранный вариант. Во время Build/Run/Debug выбор зафиксирован.
После успешного Build/Run диалог показывает путь APK и **Copy APK path**.
Release не допускает JDWP: кнопка Debug отключена, F5 явно предлагает выбрать Debug.
Compose preview пока честно помечено `debug` и не переключается вместе с APK target.

Для release настройте **фрагмент** `build` в своём `wex.json`:

```json
{
  "release_signing": {
    "keystore": "/absolute/path/to/release.jks",
    "key_alias": "release",
    "store_password_env": "WEX_RELEASE_STOREPASS",
    "key_password_env": "WEX_RELEASE_KEYPASS",
    "certificate_sha256": "<64 hex digits of the leaf certificate SHA256>"
  }
}
```

Замените placeholder настоящим SHA256 leaf certificate (`keytool -list -v -alias …`).
Разрешены 64 hex digits либо colon-separated hex. Certificate chain поддерживается:
сверяется leaf, а не CA. Keystore должен существовать вне assets/resources, чтобы
ключ не попал в APK; release key автоматически не создаётся. Путь может быть
абсолютным либо относительно проекта. JKS chain с отдельными store/key passwords
проверена настоящим SDK; аппаратные PKCS#11 providers/rotation пока не реализованы.

```sh
# В той же shell, из которой запускаются CLI/IDE; пароль не попадает в argv/history.
read -rsp 'Store password: ' WEX_RELEASE_STOREPASS; echo
read -rsp 'Key password: ' WEX_RELEASE_KEYPASS; echo
export WEX_RELEASE_STOREPASS WEX_RELEASE_KEYPASS
./target/debug/wex resolve /absolute/project --offline
./target/debug/wex build /absolute/project --variant release --offline
./target/debug/wex k2-check /absolute/project --variant release
./target/debug/wex-ide /absolute/project
```

Изменения `wex.json`, включая signing metadata, требуют обновить dependency lock
собственным resolver. Для debug signing/env credentials release не нужны. Пароли
читаются только из отдельных environment variables; raw password fields запрещены,
argv/receipts/diagnostics их не содержат. Compiler tools не наследуют configured
credential variables; signer получает их под внутренними env names, SDK diagnostics
редактируются перед отправкой в UI/записью. OS keyring/ввод пароля в GUI — дальнейшая
работа, поэтому IDE запускается из окружения с нужными переменными.

Pipeline release: actual `BuildConfig.DEBUG=false` во всех модулях, manifest
`debuggable=false`, D8 `--release`, zipalign, настроенный key, проверка v2/v3/certificate
pin и неизменности keystore, затем атомарная публикация отдельного release pointer.
Debug APK/model/pointer не подменяется. Missing credentials/key/alias, wrong password
или certificate pin оставляют прошлый release APK; нет fallback на debug key.
R8/obfuscation/resource shrinking, flavors/source-set overrides, AAB/splits пока
не входят в этот APK release path; D8 release не выдаётся за R8 shrinking.

```sh
# Изолированные ephemeral JKS keys + настоящие SDK/K2; без установки:
./scripts/check-release.sh tests/fixtures/compose-android .verification/release
# На уже запущенной mtvm; serial обязателен и не заменяется другим transport:
./scripts/check-release.sh tests/fixtures/compose-android .verification/release-vm emulator-5580
```

Suite проверяет chain2/leaf pin, independent debug, repeat-identical RSA v2/v3 APK,
6 ошибок с сохранением APK, отсутствие credentials в diagnostics, K2 definitions
из debug/release generated BuildConfig. VM-путь дополнительно проверяет настоящий
Compose frame/recomposition, выбор Release и Run в native M3 окне, свежий PID-bound
`WEX_RELEASE_OK debug=false`, отказ `run-as`, Stop/PID-gone и удаление test package.
Тестовые private keys удаляются, сохраняются только signed APKs/public certificates.
[SDK apksigner](https://developer.android.com/tools/apksigner) описывает env password
inputs и подпись после zipalign; [D8](https://developer.android.com/tools/d8) — режимы DEX.

### Android Debug: startup connection ownership

Debug prearms a direct ADB transport and `track-jdwp` before Activity launch. A
marker-drained binary Logcat stream identifies the selected package's fork from
`system_server` (PID + UID1000 + current Android user). The exact PID notification
then opens JDWP on the prepared transport, reducing the race with DDM monitors.
The completed ART handshake stays owned while Java Debug Server/JDI starts; the
relay preserves queued VM events and does not handshake ART twice. Direct ADB
sockets use TCP_NODELAY. If startup watching is unavailable, the receipt explicitly
records the exact-package PID-polling path instead. Up to three visible, same-target
cold attempts wait for PID-gone and recheck ownership; other IDEs/devices/ADB servers
are untouched. An already-owned ART connection can still fail; no debugger proxy
port guessing or forced takeover occurs.

`run.debug_transport` records `kind`, `pid`, `claim_source`, `launch_attempt` and
`claimed_before_launch_return` (the shell may finish before the process even forks;
this flag is not a claim that application code has already run). Breakpoints, step
into/over/out, complete stack, caller/object/array variables and Logcat continue to
use the existing Material 3 Expressive native controls.

```bash
# Physical check: exact USB transport; no implicit Wi-Fi substitution.
bash scripts/check-debug.sh 47101FDAS0028Q tests/fixtures/native-android /tmp/wex-usb-debug physical
# Separate explicitly virtual check when using mtvm.
bash scripts/check-debug.sh emulator-5580 tests/fixtures/native-android /tmp/wex-mtvm-debug virtual
```

A failed early `am start -D` can leave transient ActivityManager debug-app state;
repeating it may make the app its own original and leave later normal Run waiting
for a debugger. Wex checks this after target PID-gone/before launch and at Debug
Stop. It resets only the matching transient target when no persistent debug app
is selected, restores the exact presence/values of the two developer-option keys,
and verifies them. Other app selections and explicit persistent selections are
kept; conflicting temporary selections produce an explicit Debug error.

The ActivityManager fork and completed JDWP handshake can precede Android process
naming. Wex retains that connection during a bounded exact-package `pidof` recheck:
an empty early response waits; a nonempty different/multiple PID fails rather than
being guessed or attached. This prevents a successful early claim from being
discarded and relaunched merely because argv/process naming has not finished.

## Автоматический каталог Compose Preview

`wex.preview.json` больше не обязателен. `wex preview-list /absolute/project` собирает
private native snapshot только до JVM-классов/AAPT resources и читает настоящие
BINARY annotations через ASM из verified local tooling. Project classes при discovery
не загружаются/не инициализируются. Kotlin compiler уже разрешил import aliases,
const expressions, `@file:JvmName`, private top-level functions и repeat containers.
Рекурсивный meta scan поддерживает nested/custom-repeat и library multipreview;
циклы/depth/size limits дают явные diagnostics, не зависание UI.

```bash
# Для аннотированных исходников добавьте реальную ui-tooling-preview dependency
# соответствующей версии в wex.json (BOM/own resolver); synthetic annotations нет.
target/debug/wex resolve /absolute/project --offline
target/debug/wex prepare-preview /absolute/project --offline
target/debug/wex preview-list /absolute/project
# ID берётся из catalog.items[].id; уникальное имя тоже допустимо.
target/debug/wex preview /absolute/project --entry 'annotation:CATALOG_ID' --output /tmp/preview.png
bash scripts/check-preview-discovery.sh /tmp/wex-preview-discovery-check
```

В соседней M3 Expressive панели список выбирает реальные compiled scenes. Каталог
показывает group/function/source line, причину недоступности и discovery issues.
Stable ID не зависит от annotation values: Live сохраняет selection, когда unsaved
буфер меняет размеры/locale/night. Catalog/render/decode выполняются в фоне; generation
guards отбрасывают поздние results, включая каталог. Старое изображение сохраняется
при compiler/render failure или Cancel. Отсутствующее выбранное ID — явная ошибка,
не переключение на первую сцену. Duplicate names требуют ID; qualified labels различимы.

Поддержаны explicit widthDp/heightDp (1..1080/1..1920), day/night `uiMode` (с NORMAL
или без type bits), fontScale0.5..3, locale, showBackground/32-bit ARGB. Locale передаётся
в настоящий Layoutlib resource configuration, background компонуется за прозрачным
render. Default annotation dimensions используют выбранный default viewport360x640,
а не natural wrap-content. Optional manual config сохраняет прежний default layout.

Parameter providers/default-argument methods, instance members, hardware `device`
presets, system decor, wallpaper/dynamic colors и отличающийся apiLevel пока остаются
видимыми disabled entries с причиной. Unknown attributes не игнорируются. Это не
утверждение полного Studio feature parity. Closed-file idle watch, source-level/K2
preview discovery, incremental compilation, natural wrap sizing и interactive clock
остаются частью общей цели IDE. APK current.json/DEX/sign/install не затрагиваются.

### Debug: целостность просмотра переменных

Отладчик проверяет ответы DAP до обновления панели: отсутствие `scopes`/`variables`,
некорректные поля и неполная страница массива показывают ошибку, а не пустой список.
Уже загруженные данные остаются на экране; **Retry variables** повторяет загрузку
текущего кадра или страницы. Последняя страница массива запрашивает и принимает
ровно оставшееся число элементов. Запоздалые ошибки другого кадра/предыдущей
страницы не меняют текущую инспекцию. Кнопки повторной загрузки используют общие
Material 3 Expressive цвета, формы и прерываемые анимации IDE.

Подтверждения точек останова проверяют весь список файла; непроверенная точка
остаётся непроверенной, а строка не переполняется при преобразовании из DAP.
Физическая проверка по USB выполняется отдельно и строго с указанным serial:

```sh
bash scripts/check-debug.sh 47101FDAS0028Q
```

Если этот transport отсутствует, проверка останавливается **до сборки/установки**;
Wi‑Fi и эмулятор не подставляются автоматически. Host-регрессия проверяет JDK/JDWP
и живое окно IDE, но не является подтверждением отладки Android на телефоне.


## Native K2 analysis до первой сборки APK

```sh
cargo run --locked --bin wex -- analyze /path/to/project --offline
cargo run --locked --bin wex -- analyze /path/to/project --variant release --offline
cargo run --locked --bin wex -- sync /path/to/project --offline
bash scripts/check-analysis.sh .verification/analysis
```

`wex-native-analysis-3` отделён от APK-модели. Модель хранится в
`.wex/analysis/models.lmdb`, указатели — в `.wex/analysis/debug|release/current.json`.
Сначала собственный resolver проверяет `wex.lock.json` и cached JAR/AAR, затем
AAPT2/Javac создают Android-символы. Генерация останавливается перед компиляцией
пользовательского Kotlin/Java, DEX, подписью и установкой. Release-анализ имеет
настоящий `BuildConfig.DEBUG=false` и работает без release keystore; сборка release
APK по-прежнему требует явно настроенной подписи.

Отпечаток учитывает конфиг/lock, ресурсы/манифесты, SDK/JDK и compiler inputs, но не
тела пользовательских Kotlin/Java-файлов и assets. Поэтому анализ повреждённого кода
продолжает работать, а изменение ресурсов перестраивает R. `executed_generation_steps`
равно0 при попадании в кэш; `model.generation_steps` описывает историческую генерацию.
Каждый сохранённый файл, полный inventory и LMDB graph проверяются по хэшам.
Ошибки AAPT2 не заменяют предыдущий указатель. Неизвестный/повреждённый кэш
перестраивается, а старые поколения сохраняются для уже работающих LSP-сессий.

Проверка использует настоящий K2 на новом Android-проекте и настоящий AAPT/Javac
даже при отсутствии D8/signers/keytool. Cache-hit отдельно проходит с полностью
снятыми execute-битами **приватных копий** SDK/JDK tools; оригинальные SDK/JDK не
меняются. Проверяются ошибки Kotlin/Java, новый ресурс, повреждение ресурсов,
generated sources, лишний файл и потерянная LMDB запись, debug/release и force.
Живое GPU-окно само готовит первую модель, показывает реальную ошибку типа и Undo,
переключает Release штатной кнопкой и через F12 открывает generated BuildConfig.
Инициализация соединения сама по себе не считается готовой диагностикой: проверка
ожидает ответ для точного пути и версии буфера.

Анализ, APK и Preview используют объявленные module-scopes для classpath и
non-transitive R по собственным полям. Ресурсные значения выбранного приложения
по-прежнему объединяются в одну Android-таблицу с overlay semantics, но это не
делает чужие поля частью собственного R. Недостижимые из приложения модули теперь
анализируются в отдельных resource contexts; idle resource/config watching, GC
поколений и инкрементальная компиляция остаются работой.


## Области зависимостей модулей

`wex.lock.json` resolver ABI3 содержит `module_scopes`: набор выбранных Maven GA,
достижимых от зависимостей каждого модуля с учётом **конкретного пути exclusions**.
Это отдельные данные: объединённые `artifacts[].dependencies` подходят для просмотра
общего графа, но не заменяют scope, если другой модуль требует исключённый artifact.
Версии по-прежнему выбираются согласованно для всего workspace; это не per-module
медиация разных версий одной библиотеки.

Для Kotlin/Java compiler и K2 модуль получает собственные JAR/AAR, транзитивные
зависимости и closure явно объявленных `projects`. Выбранный Kotlin stdlib/runtime
считается общим языковым platform dependency; другие зависимости приложения не
становятся зависимостями библиотеки автоматически. Ранее собранные sibling outputs
не добавляются в classpath без project edge.

APK/Preview извлекают и упаковывают только closure выбранного приложения. Недостижимый
AAR не попадает в DEX, manifest, resources или assets. Заголовки R/BuildConfig
создаются в отдельных scoped JAR; namespace родителя не копируется в библиотеку,
а копирование пакета не захватывает дочерние namespaces рекурсивно. Набор R-полей
теперь определяется собственными ресурсами namespace; значения ID берутся из
окончательной linked Android resource table (подробности ниже).

`--locked` остаётся read-only и требует актуальный ABI. Обычный Sync умеет обновлять
метаданные. Build обновляет только intact ABI2 lock с неизменённым конфигом **offline**,
проверив все старые хэши до миграции; изменения зависимостей требуют Resolve/Sync.
Toolchain scope-lock хранится в отдельном `scopes-v3`, не меняя legacy toolchain cache.
Оригинальные байты конфига связываются с resolve/build/analysis, чтобы смена JSON во
время подготовки не публиковала модель от другого конфига. Неявный annotation
processing отключён (`-proc:none`); отдельные KAPT/KSP/processor конфиги ещё предстоят.
Offline допускает чтение локального `file:` Maven repo, но не сетевые обращения.

```sh
bash scripts/check-module-scope.sh .verification/module-scope
```

Проверка компилирует настоящие fixture JAR/AAR через JDK17 target, использует SDK/K2
и проверяет parent-only зависимость, sibling output и contextual Maven exclusion:
ошибка до явного объявления, успешная сборка после; недостижимые классы/assets/
ресурсы исключены из APK. Реальные K2 случаи запускаются opt-in ignored integration
тестом, а не fake JSON-RPC. Material3 Expressive интерфейс использует тот же backend.

## Собственные поля ресурсов / non-transitive R

Как и [Android non-transitive R](https://developer.android.com/build/optimize-your-build#use-non-transitive-r-classes),
`R` проекта содержит только объявленные им ресурсы. Например, приложение с `projects:
["shared"]` обращается к ресурсу библиотеки через `dev.example.shared.R.string.title`,
а не через свой `R.string.title`. Само объявление зависимости не переносит чужие
поля в R приложения. Родительский namespace также отсутствует в библиотечном classpath.

После общей AAPT2-связки Wex отдельно связывает **только собственные compiled inputs**
каждого namespace как static library, используя окончательную Android-таблицу для
разрешения ссылок. Manifest этой связки использует applicationId таблицы, а
`--custom-package` — namespace Java R: различающиеся имена не ломают XML aliases
на ресурсы объявленной библиотеки. Полученный AAPT `R.txt` определяет ownership: values, file resources,
ID из `@+id`, aliases, styles/arrays и styleable-группы. XML-regex и вымышленных ID нет.
`compile --dir --output-text-symbols` для inventory не используется: SDK перезаписывает
его при каждом входном файле. Это обнаружено настоящей проверкой layout + values.

Генератор подставляет финальные ID только в собственные поля. `styleable` получает
свой массив и свои индексы, даже если другое пространство имён объявило группу с
тем же именем: общая объединённая форма массива не копируется. Для precompiled AAR
индексы берутся из его `R.txt`, поскольку компилятор мог встроить их в bytecode;
содержимое массива заново связывается с финальными attr IDs. Несовместимый/неполный
binary contract, отсутствующий `R.txt` у AAR со styleables, одинаковый namespace
проектных модулей или project/AAR namespace collision завершают подготовку ошибкой,
не заменяя last-good модель/APK. AAR с общим namespace объединяет совместимые
binary contracts; конфликт формы/индексов не замалчивается. Генератор кэша обновлён
до `workspace-contexts-v7`, старые модели автоматически перестраиваются.

```sh
bash scripts/check-resource-scope.sh .verification/resource-scope
```

Проверка использует реальные AAPT/Javac/Kotlin/K2/D8/signers: ошибки parent/shared R,
правильные qualified ссылки, layout ID, overlays, совпадающие имена styleable, AAR
с намеренно отличающимся binary index order. Четыре настоящих K2 negative/recovery
случая ждут completion собственного поля, затем соответствующие diagnostics;
JDK реально исполняет precompiled AAR. Java-ошибка, испорченный XML и namespace
collision сохраняют подписанный APK pointer; исходники/конфиг теста восстанавливаются.
Runtime проверен на явно выбранной `mtvm` через CLI и штатную кнопку Run: resource
имена/строки/overlay/styleable формы проверяет само приложение, Logcat связан с PID/runId.
Compose K2, настоящий headless Layoutlib и M3 GPU-окно проверяются отдельно.

## Анализ всего workspace, а не только приложения

`analyze` / Sync / K2 публикуют **каждый модуль из wex.json**, в том числе ещё не
подключённую Android-библиотеку или Kotlin-модуль без namespace. Первая область
остаётся dependency closure выбранного приложения. Затем Wex выбирает отдельные
dependency-closed корни, покрывающие оставшиеся модули; общий project dependency
публикуется один раз. Библиотека не может зависеть от application-модуля.

Каждый дополнительный корень получает настоящую SDK-связку только своих AAR,
resources и project dependencies. Манифест приложения, его строки/компоненты и
ограничение runtime minSdk не подмешиваются в независимую библиотеку. Для private
library analysis link используется ceiling compileSdk: это не подтверждение
совместимости библиотеки с minSdk приложения. Библиотечный manifest без application
допустим; пустой application carrier добавляется только в private resource link.
Обычная APK-сборка по-прежнему требует настоящий application manifest.

Граф сохраняет реальный `LIBRARY` type, namespace (включая пустой), исходники Java/
Kotlin, module edges и scoped classpaths. У namespace-less Kotlin-модуля нет
придуманного собственного R/BuildConfig. Leaf R виден только по объявленному edge.
Library analysis R IDs привязаны к конкретной SDK-связке, а не к ещё не собранному
APK; настоящая сборка заново получает ID выбранного application closure.

Модель `wex-native-analysis-3`, receipt schema2, содержит `contexts`: root module,
dependency closure, уникально назначенные модули и generation directory.
Дополнительные деревья перемещаются внутрь одного owned generation `contexts/<root>`;
переадресуются только generated/classpath пути, не user source paths и не исторические
аргументы уже выполненных SDK-команд. Inventory/LMDB hashes покрывают весь результат.
При каждом cache-hit и до публикации проверяются полное покрытие конфигурации,
root/closure/assignment, типы, namespace, source/dependency mapping и точные generated
roots. Все contexts связываются с одинаковыми captured config/lock bytes.
Compose plugin metadata объединяется для всех модулей с проверкой идентичности tools.

Analysis не компилирует пользовательские Kotlin/Java тела, DEX или подписанный APK.
Source errors не ломают модель. Ресурсная ошибка пока отклоняет новую **целую** модель
и сохраняет last-good pointer/generations; частичные cold-start модели и per-context
incremental regeneration остаются работой. Повторная полная генерация нескольких
contexts не считается инкрементальной компиляцией или оптимизированным Sync.

Build/Run/Preview собирают только приложение и его closure. Неиспользуемые source/
resource/asset тела больше не входят в их input fingerprint; общий wex.json/lock
по-прежнему входит, потому что версии Maven согласуются на весь workspace. Даже
ошибка Kotlin/XML в detached библиотеке не добавляет её в APK и не блокирует его
компиляцию. Это не отменяет проверки общей конфигурации/lock и объявления зависимостей.

```sh
bash scripts/check-workspace-analysis.sh .verification/workspace-analysis
```

Проверка использует реальные SDK/JDK/K2: пять модулей, три отдельных contexts,
отдельный Maven AAR с minSdk31 при app minSdk26, Java source definition, completion,
недоступный parent R и recovery, namespace-less Kotlin → leaf R. Cache-hit проходит
со снятыми execute-битами приватных SDK/JDK копий. Проверяются coverage tamper,
ресурсная ошибка/last-good, debug/release и native APK с исключёнными detached
классами/resources/assets. Native-window probe может выбрать project-relative Kotlin
через `WEX_ANALYSIS_SMOKE_SOURCE` вместе с `--analysis-smoke`: реальный первый Sync,
ошибка/Undo, штатный Release picker, F12 generated DEBUG=false; никаких fake LSP events.

## Живое обновление native Android-моделей и K2

Native-редактор следит за `wex.json`, `wex.lock.json`, manifests и ресурсами **всех**
объявленных модулей, а также фактическими SDK/JDK/compiler/dependency inputs.
Проверка выполняется в background worker раз в 500 ms; серия изменений объединяется
с debounce 650 ms. Отпечаток содержимого учитывает создание, удаление и atomic replace;
обычное изменение timestamp при одинаковых байтах не перезапускает анализ. Memo по
метаданным уменьшает повторное чтение больших JAR. User Kotlin/Java тела и assets не
вызывают SDK Sync: dirty active/parked buffers отправляются работающему K2 напрямую.
Это polling, не нативный OS file-event watcher, и не инкрементальный компилятор.

Новая модель готовится отдельно. Новый настоящий K2 получает собственные workspace
JSON и IntelliJ system directory, все подходящие несохранённые буферы, затем проходит
реальный import barrier и первый diagnostic checkpoint. Только актуальная пара
model revision + editor snapshot заменяет сессию. Устаревшая работа отменяется;
UI показывает подготовку/ошибку, сохраняя прежний анализ, код, курсор и Undo. Старый
K2 остаётся доступен для completion/definition/diagnostics, пока кандидат прогревается
или ресурсы/конфигурация содержат ошибку. Project rename/references и импортирующие
completion edits во время stale/updating состояния требуют свежей модели.

Исправление внешнего файла автоматически повторяет подготовку. При неизменной ошибке
нет бесконечного retry loop: footer и Project model settings предлагают **Retry**.
Debug/Release picker прогревает нужный вариант без потери буферов. Удаление wex.json
в уже native-сессии не запускает Gradle fallback. Файл, исключённый из нового графа,
остаётся в редакторе и явно отмечается как outside model, без вымышленных diagnostics.
Configured release credential environment names удаляются из окружения дочернего K2;
значения не читаются и не попадают в workspace JSON или argv.

```sh
bash scripts/check-model-reload.sh .verification/model-reload
# Fresh private fixture, real native M3/wgpu window, keyboard/pointer + external XML:
python3 tests/model_reload_e2e.py .verification/model-reload-window-fixture
ANDROID_HOME="$PWD/.verification/model-reload-window-fixture/sdk" \
JAVA_HOME="$PWD/.verification/model-reload-window-fixture/jdk" \
  target/debug/wex-ide .verification/model-reload-window-fixture/project \
  --model-reload-smoke .verification/model-reload-window
```

Первый тест использует настоящие SDK и K2, приватные копии executable tools, source
edit при снятых execute-битах, external resources/config/module edges, malformed и
удалённый config, last-good completion и Release definition. Второй тест делает
настоящие raw keyboard/pointer действия и девять GPU screenshots: dirty parked
семантика, automatic resource refresh, ошибка/Retry, внешнее исправление, Undo,
Release picker/F12. Дисковые Kotlin-исходники остаются исходными, APK не создаётся.

Частичная cold-start recovery по contexts, per-context reuse, generation GC,
инкрементальная APK-сборка и оптимизация polling большого workspace остаются работой.
Старая Gradle-совместимость не получила этот native watcher.

## Повторное использование Kotlin/Java по модулям

Native APK-сборка сохраняет проверенные выходы компиляции в project-local
`.wex/compile-cache/wex-module-classes-v1/<variant>/<module>/<key>`. Неизменённый
модуль копирует свои `.class` и Kotlin module metadata в **новое** build directory,
а не ссылается на изменяемый output прошлой сборки. `BuildReport.compilation_cache`
показывает `hit` / `miss` / `disabled`; `steps` содержит только действительно
исполненные команды. Панель Build показывает Material 3 Expressive карточку
**Last successful build**, число повторно использованных модулей и их состояния.
Она не выдаёт прежнюю успешную сборку за успешную текущую после ошибки.

Ключ включает source paths/bytes, порядок и содержимое scoped classpath, выходы
project dependencies, Kotlin compiler/plugin bytes, variant/JVM target/Compose flags,
фактические JDK launchers/bin и используемые runtime libraries/configuration,
значимые JVM/locale/PATH environment inputs. Значения release credential references
не входят: компилятор их не наследует. Временные пути SDK symbol JAR не входят в ключ;
JAR fingerprint учитывает ordered names/uncompressed payload, не timestamp/compression.
Это позволяет новым настоящим AAPT/Javac R/BuildConfig headers совпадать по содержимому.
Повреждённые/лишние/удалённые cache files отклоняются; модуль компилируется заново.
Публикация cache receipt проходит через owned temporary directory и замену целого entry.
Восстановленные файлы — отдельные копии, без hardlinks. Inputs перепроверяются после
компиляции/restore и перед публикацией APK; ошибка сохраняет предыдущий APK pointer.

Изменение приложения не перекомпилирует неизменённую библиотеку. Изменение bytecode
библиотеки консервативно инвалидирует зависимые модули, даже без изменения public ABI.
Удалённые source classes отсутствуют в новом APK. Изменение resource value при
неизменных R IDs может использовать прежние классы; новые R fields инвалидируют
соответствующие scoped consumers. Compose cached classes сохраняют настоящее lowering,
matching plugin metadata и список модулей. При `compose.reports=true` cache отключён:
реальный compiler заново создаёт metrics/reports, отсутствующие в class-output cache.
Private Preview snapshots пока тоже компилируются без этого кэша; analysis-only путь
по-прежнему не компилирует пользовательские тела.

```sh
bash scripts/check-compile-cache.sh .verification/compile-cache
python3 tests/compile_cache_compose_e2e.py .verification/compile-cache-compose
# Native keyboard/pointer Build/Save/Edit/Undo; use the first suite's private JDK/PATH:
JAVA_HOME="$PWD/.verification/compile-cache/jdk" \
PATH="$PWD/.verification/compile-cache/traps:$PATH" \
  target/debug/wex-ide .verification/compile-cache/project \
  --compile-cache-smoke .verification/compile-cache-window
```

Проверки используют настоящий SDK/Kotlin/Java/DEX/signing, независимые wrapper counters
для реальных compiler launches, body/dependency/source removal/resource/JDK/JVM option
изменения, damaged cache и compilation failure. Compose проверяется по настоящему
`javap` bytecode и APK DEX, а не по наличию plugin option. Native smoke нажимает Build
четыре раза настоящим pointer input, меняет код клавиатурой и восстанавливает Undo;
сохраняет пять GPU screenshots и реальные BuildReports.

Это **module-level class reuse**, не Kotlin FIR incremental compilation, не ABI-only
invalidation и не up-to-date APK shortcut. Сам по себе module cache не пропускает AAPT, generated R/JDK image, D8, packaging,
signing и signature/alignment verification пока выполняются при каждой APK-сборке.
Cache GC, shared Preview reuse, per-file incremental compilation и оптимизация
повторного hashing больших classpaths/JDK остаются работой. Cache fingerprint ABI
нужно менять при изменении compiler invocation semantics; unsupported cache inputs
отключают оптимизацию, но не заменяют компиляцию вымышленным успехом.

## Общий кэш Android Java image (SDK-этап)

Build, все analysis resource contexts/variants и private Preview snapshots используют
проверенный `android-java-image-v1` в `.wex/sdk-cache`. Ключ связывает фактические
SDK core bytes, JDK launchers/backing files/runtime libraries/configuration,
OS/architecture, JVM/PATH/locale inputs и точные descriptor/JMOD/Jlink flags.
Новая модель также учитывает эти JDK inputs и эффективное tool environment: это не
причина повторять Sync при обычном изменении Kotlin/Java тела.

При попадании в кэш четыре реальные команды — java.base descriptor, module JAR,
JMOD и Jlink — не запускаются. Проверенные image files копируются в owned work tree;
Javac `--system` никогда не указывает на изменяемый shared cache. Private Preview
использует кэш исходного проекта, но сохраняет отдельную копию image в каждом snapshot.
Ни user classes, ни APK, ни новый ресурсный context этим кэшем не подменяются.

Unix per-key OS lock защищает публикацию APK/analysis/Preview между процессами.
FD закрывается при cancellation/crash; persistent lock filename не означает занятую
блокировку. Busy-cache не задерживает UI: запрос строит независимый image без записи
в занятый entry. Missing/corrupt/unrecorded payload повторно генерируется настоящим
SDK. Полный inventory/hash проверяется до/после копирования; inputs перепроверяются
после генерации и до публикации model/APK/Preview. Release signing preflight остаётся
отдельным и не пропускается.

`BuildReport.sdk_cache` и Preview/Catalog `sdk_cache` описывают текущий SDK вызов.
Analysis `model.generation_sdk_cache` — **исторические** результаты generation,
как `generation_steps`: при полном model cache-hit текущих SDK-команд нет вообще
(`executed_generation_steps=0`). В Build M3-карточка отдельно показывает состояние
Android Java image. Старые JSON receipts читаются с пустым default metadata.

```sh
bash scripts/check-sdk-cache.sh .verification/sdk-cache
python3 tests/sdk_cache_preview_e2e.py .verification/sdk-cache-preview
```

Проверки используют настоящие tools и независимые launch counters: cold/warm,
analysis→release→APK, damaged/extra image files, SDK/JDK/options changes, реально
занятый OS lock и сохранение прошлых результатов при ошибке ресурсов. Preview
проверяется настоящим Compose/Layoutlib portrait/night/catalog, разными private
`--system` путями и нулём повторных SDK transform launches. Это кэш одного SDK
transform, не whole-APK up-to-date shortcut и не FIR incremental compilation.
AAPT/scoped R generation/DEX/signing остаются работой каждой APK-сборки. AAPT tree
reuse, per-context incremental models, cache GC и большие performance targets открыты.

## Инкрементальная компиляция ресурсов по файлам

Native engine сохраняет настоящие AAPT2 `.flat` в project-local
`.wex/resource-cache/aapt-flat-files-v1`. Ключ связывает SDK tool/backing binary/native
libraries/effective environment, namespace, исходный resource origin, qualifiers/name
и байты файла. Нормальные `res/type[-qualifier]/file` компилируются батчами до128 misses;
warm-файлы не запускают AAPT compile. Новый ZIP собирается **только** из текущего
списка файлов в исходном порядке, поэтому удаление/rename не оставляет stale resources.
AAPT link и owner-only static links всегда выполняются и заново назначают IDs/проверяют
references/overlays/styleables. Это не кэш APK или финальной таблицы ресурсов.

Для одинаковых AAR/project resources между builds/analysis/Preview используются
immutable content-addressed inputs в `.wex/resource-inputs`. Эти проверенные копии
избавляют `.flat` от ссылок на исчезнувшие AAR/Preview temp directories. `origin.json`
связывает копию с исходником; diagnostics отображают исходный project path или
`artifact.aar!/res/...`. Реальные argv и успешные Step stdout/stderr не переписываются.
Preview использует origin исходного проекта и owned snapshot-выходы, не APK pointer.

Проверяются точный payload inventory/hash, source snapshot, inputs до/после compile и
перед publication. Corrupt flat повторно компилируется; damaged source snapshot
восстанавливается отдельно. Per-file Unix OS lock освобождается при exit/cancel;
занятый файл компилируется независимо в owned work copy, не заменяя занятый entry.
Нестандартные resource shapes используют настоящий directory compile, явно uncached,
а не упрощённую модель. Metadata counts охватывают все файлы; `samples` ограничен8
диагностическими примерами, не является полным списком ключей.

`BuildReport.resource_cache` и Preview/Catalog `resource_cache` описывают текущие
вызовы. `model.generation_resource_cache` хранит историческую generation, как SDK stats.
Генератор модели теперь `resource-files-v8` (API3/schema2): старые модели перестраиваются.
M3 Build card показывает общее число реально повторно использованных resource files.

```sh
bash scripts/check-resource-cache.sh .verification/resource-cache
python3 tests/resource_cache_preview_e2e.py \
  .verification/resource-cache .verification/resource-cache-preview
```

Реальные AAPT counters проверяют cold/warm, один изменённый XML, qualifier add/delete,
rename, PNG/raw/layout IDs, damaged cache/source, OS-lock fallback, tool changes и
ошибки compile/link с сохранением APK. Отдельно проверяются actual K2 resource scopes,
precompiled AAR styleable index ABI, настоящий Compose/Layoutlib и mtvm APK runtime.
Per-context partial generation, cache/source GC и performance
больших проектов остаются работой. Cold batching — не инкрементальный Kotlin compiler.
Основа SDK режима: [официальная документация AAPT2](https://developer.android.com/tools/aapt2).


## Проверенный кэш DEX

Native APK build хранит настоящие результаты D8 в
`.wex/dex-cache/d8-dex-v2/<key>/dex`. Изменение только значения ресурса или assets
повторно связывает и подписывает APK, но не запускает D8, если bytecode не изменился.
Новый/изменённый bytecode, порядок program/boot JARs, min API, Debug/Release,
SDK D8/script/backing/JAR/native libs, JDK runtime или JVM options меняют ключ.
JARs сравниваются по упорядоченным uncompressed entries, не ZIP timestamps.
Учитываются и настроенный JDK, и Java, фактически выбранная SDK-скриптом через PATH.
Динамические shell hooks (`BASH_ENV`, `ENV`, exported Bash functions) отключают
этот кэш: D8 выполняется по-настоящему без публикации неполного ключа.

Receipt проверяет точный inventory/hash. Каждый build получает собственные копии;
заголовки DEX и последовательность `classes.dex`, `classes2.dex`… проверяются отдельно.
Это базовая проверка формата, не полная ART-верификация bytecode. Повреждение/лишний
файл приводит к регенерации. Per-key OS flock не блокирует UI: занятый ключ вызывает
независимый D8 и не перезаписывает чужую запись. Входы повторно проверяются после
D8/restore и перед публикацией APK. Ошибка D8 сохраняет last-good APK/current pointer.

`BuildReport.dex_cache` описывает текущий вызов (`hit`, `miss`, `disabled`);
`steps` содержит D8 только при фактическом запуске. Material 3 Build card показывает
DEX Reused/Regenerated/Uncached рядом со статистикой модулей/SDK/ресурсов.
Поле `files` считает подтверждённые файлы кэша (0 при disabled или неудачной записи).
Analysis и Preview по-прежнему
останавливаются до DEX, подписи и устройства. Финальная AAPT-связка, owner static
links, packaging, alignment, signing и verification всегда настоящие; whole-APK
shortcut, R8, AAB, FIR/per-file incremental compiler и cache GC этим не реализованы.
Настроенный release key внутри assets/resources отклоняется также в Debug/analysis
и до копирования Preview snapshot; отсутствующий внешний release key/credentials
для Debug/analysis не требуется.

```sh
bash scripts/check-dex-cache.sh .verification/dex-cache
# Только явно выбранная виртуальная машина; physical USB остаётся отдельной проверкой.
target/debug/wex run .verification/dex-cache/project --serial emulator-5580 \
  --offline --log-seconds 4 --require-marker WEX_DEX_CACHE_OK
```

Сценарий использует реальные SDK tools с независимыми счётчиками запусков. Он
проверяет warm/resource/asset reuse, source/dependency/new/deleted class, повреждения,
OS-lock, tool/backing/JAR/JDK/options/minAPI и настоящий duplicate-type отказ D8.


### Java, выбранная D8 относительно проекта

DEX ABI `d8-dex-v2` связывает папку запуска SDK-команды и реальный Java runtime.
Относительные и пустые PATH entries отсчитываются от проекта, даже если IDE/CLI
запущена из другой папки. Первый исполняемый Java выбран по порядку PATH; symlink
раскрывается для проверки launcher/runtime bytes. Изменение постороннего JDK в папке
запуска IDE не сбрасывает DEX, изменение реально выбранного launcher сбрасывает.
Старая ABI1 может содержать ключи с неверной привязкой относительного PATH и не
читается. Сам SDK получает исходный PATH: конфигурация запуска не переписывается.

```sh
bash scripts/check-dex-path.sh .verification/dex-path
```

Проверка противопоставляет caller/project JDK и использует настоящие D8/Java
вызовы с независимыми счётчиками. SDK Java wrapper переключает реальные D8
`--debug`/`--release`: кэш должен сохранить различающиеся DEX, а не старую копию.
Также проверяются пустой PATH entry, пропуск неисполняемого кандидата и настоящий
uncached build при launcher без проверяемого runtime layout. Это не поддельный
компилятор или подмена DEX; все APK подписываются и проверяются SDK-инструментами.

Native tool roots ANDROID_HOME/JAVA_HOME resolve once against the IDE/CLI launch
context and become absolute before child cwd changes. D8 PATH search remains relative
to project cwd. Test relative JAVA_HOME with distinct caller/project JDKs: every
actual tool argv must be absolute and use the configured canonical JDK; D8 still
uses the project PATH Java. No silent mixing of key JDK and executed JDK.

K2 workspace SDK homePath and WEX_DEBUG_JAVA_HOME normalize their configured
caller-relative roots too, before import/adapter child cwd changes. Real K2 check
under relative JAVA_HOME must import the same canonical JDK as native compilation;
Android Debug validation uses a caller-relative debugger JDK with explicit VM serial.

Native analysis SDK context revision tool-root-bindings-v2 invalidates prior full-model
receipts that could have fingerprinted caller JDK bytes but executed project-relative
JAVA_HOME. Symbol generator resource-files-v8/API3/schema2 remain unchanged; this is
a tool-binding input revision, not a changed resource namespace/schema.

## Create / Open native Android projects

Click **W** in the app bar (or **New / Open project** in workspace settings).
The Material 3 Expressive form creates Kotlin Views, Java Views or a Compose
Material Expressive starter, including pinned `wex.json`, a launcher Activity,
resources, manifest and optional real Layoutlib Preview configuration. Choose the
parent directory, new folder, package and minimum SDK (23–36). Compile/target SDK36,
build-tools36.0.0, JDK21 and Kotlin2.4.0 are explicit template toolchain pins.
Creation performs no dependency downloads or build; opening starts normal native
analysis/K2. Dependencies resolve through Wex, not Gradle. Package validation and
Android/XML app-name escaping apply to both the form and CLI.

```sh
wex new /absolute/parent --name "My Application" --directory MyApplication \
  --package com.example.myapplication --template compose --min-sdk 26
```

Generation uses a private sibling staging directory, validates the configuration,
and atomically publishes without replacement using Linux `renameat2(RENAME_NOREPLACE)`.
Existing files, directories (including empty ones) and symlinks are never replaced.
Linux is currently the supported publication backend. Generated files contain no
keys or release signing configuration. No `wex.lock.json` is fabricated.

**Open in new window** keeps current unsaved/parked buffers and Undo intact. Existing
native projects can be opened by their directory. Tool roots retain the IDE launch
cwd across the new window; the project is a separate absolute argument. Input path
validation runs on background workers; stale input responses are dropped. The actual
Create/Open operation rechecks state. Additional module/template management remains
future work; Recent and the in-app folder chooser are described below.

Verify: `scripts/check-projects.sh OUTPUT` (three genuinely compiled/signed Android
apps, warm caches, escaped names, real Compose/Layoutlib) and native `--project-smoke`
(form pointer/keyboard, invalid recovery, two actual child editors with K2, dirty/Undo
preservation). Device runtime verification requires an explicitly named mtvm transport;
this feature never auto-selects a phone for tests.

### Recent projects and folder chooser

The project dialog now has **New / Open / Recent** pages. **Browse folders** opens
an in-app Material3 directory chooser: address + Go, Up/Home/Refresh, hidden-folder
switch, Unicode name filtering and virtualized rows. **Use this folder** only fills
the form; **Cancel** preserves the old path. Done/Cancel or Escape dismiss the
respective dialog; backdrop clicks do not discard a form during resizing/tab changes. A `wex.json` badge means the file exists,
not that its configuration has been validated. Actual Open rechecks the config on
an opener worker before spawning a separate IDE; current documents/Undo stay intact.

Successful native startup and Open remember the canonical root locally. Recent
entries can be filtered/opened/removed; removal only edits metadata, never project
files. Missing/moved projects stay visible and removable. Refresh reads changes
from other windows. Malformed/unsupported state is reported and preserved rather
than silently replaced; the UI keeps its last-good list until explicit repair/retry.
The file holds at most24 paths/names/timestamps (no code, credentials or device data).

Storage defaults to `$XDG_STATE_HOME/wex/recent-projects.json`, or
`$HOME/.local/state/wex/recent-projects.json`. `WEX_PROJECT_HISTORY` overrides it
(and is required to isolate integration test history). Reads do not create state;
writes use a bounded real Unix file lock, merge latest state, temp-file/file+directory
sync and atomic rename. Non-cooperating external edits are checked before publication.
This is not a cross-platform storage backend or live cross-window file watcher.

```sh
wex recent --history /absolute/state/recent-projects.json
wex recent --history /absolute/state/recent-projects.json --add /absolute/project
wex recent --history /absolute/state/recent-projects.json --remove /stored/absolute/root
wex browse /absolute/directory --hidden
```

Folder enumeration is bounded (4096 folders /20000 scanned entries) and explicitly
reports partial/skipped-entry results. Folder I/O, path validation, project-opening
preflight and history I/O run off the GPU thread; stale folder receivers are dropped.
Verify `scripts/check-project-navigation.sh OUTPUT`: eight concurrent real CLI writers,
malformed-history preservation, directory cap, native chooser/Recent pointers in two
separate IDE processes, three actual K2 child editors, and dirty/Undo preservation.

## Native Linux release package

`cargo build --release --bins --features desktop --locked --offline` builds the native
client with size optimization, thin LTO, stripping and `panic=abort`. Ordinary tool/
config/protocol failures remain Results; unexpected Rust panics terminate the process.
The package gate is **20,000,000 bytes for the uncompressed wex-ide ELF**, not compressed
archive size. CLI, support sources, combined payload and archive bytes are reported
separately; Android SDK/JDK, Kotlin server/compiler caches and Layoutlib are external.

```sh
python3 scripts/package-linux.py --output /absolute/output/wexstudio-linux-x86_64
/absolute/output/wexstudio-linux-x86_64/wexstudio
/absolute/output/wexstudio-linux-x86_64/bin/wex doctor --require-support
```

The relocated package contains native CLI/IDE, a Material3 launcher/icon/menu template,
and hash-inventoried helper sources. It contains no SDK, JVM, server JARs, compiler
JARs or prepared classes. `WEX_SUPPORT_HOME` selects immutable support; `WEX_TOOLS_HOME`
selects writable tool data. Packaged defaults use adjacent `share/wex` and
`$XDG_DATA_HOME/wex/tools` / `$HOME/.local/share/wex/tools`. Roots resolve once in caller
context, before child project cwd changes. A release binary with missing packaged
support reports that missing location instead of falling back to a build checkout.
Development debug binaries retain the existing repository/.wex default.

For existing prepared tools, set `WEX_TOOLS_HOME=/absolute/checkout/.wex` explicitly.
`wex prepare-lsp --offline` verifies/reuses the pinned cached K2 archive; without the
flag the explicit preparation command may fetch that archive. Welcome launch never
provisions, resolves or downloads. `doctor` reports availability and support integrity,
not proof that arbitrary projects can build. Optional menu installation is explicit:
`python3 PACKAGE/install-desktop.py --applications /absolute/applications-directory`;
it creates a new shortcut without replacing files or changing associations/autostart.

`wex-ide --welcome` opens a true projectless screen: no source discovery/documents,
project preferences, device query or K2 startup. Normal no-argument launch outside a
recognized project also uses it. An explicit project directory or wex.json file opens
its workspace. The package launcher starts welcome when called without arguments and
preserves caller cwd for relative JDK/SDK/tool configuration. Native UI and CLI runtimes
use two async workers; compiler/SDK work retains its existing off-UI execution.

Verify `tests/release_package_e2e.py ARCHIVE OUTPUT`: relocate through a path with spaces,
manifest/tamper/missing-support checks, explicit private desktop entry, three real GPU
welcome frames from a 10,000-file cwd (zero discovery/K2/devices), offline K2/Android
semantics, signed native APK/error recovery and genuine Compose/Layoutlib. Device
Run/Debug validation requires explicit mtvm serial; production byte/latency measurements
must be reported as observed, not inferred from build success or called Studio parity.

### Большая история: освобождение вне потока интерфейса

Desktop-редактор передаёт последние ссылки на большие Rope/Tree-sitter состояния
и использованные парсеры одному ленивому потоку освобождения. Это обычное владение
и `Drop`, без трассирующего GC: отмена ветки Redo, вытеснение истории и закрытие
вкладки больше не должны обходить большое дерево на потоке GPU. Общие версии
остаются живыми, пока ими пользуются Undo, Save, фоновые операции или другие вкладки.
Маленькое замещение большого файла сохраняет признак тяжёлого синтаксического seed.

Проверка в настоящем окне:

```sh
CARGO_BUILD_JOBS=2 bash scripts/check.sh
bash scripts/check-history.sh /absolute/path/to/new-test-directory
```

Проверка создаёт настоящий Kotlin-файл на100000 строк, вводит текст, выполняет
Undo/Redo, переключает и закрывает вкладку, затем открывает файл снова. Семь
GPU-снимков и JSON подтверждают разные потоки ввода/освобождения, фактическое
освобождение дерева и парсера, пустую очередь и неизменность исходных файлов.
`elapsed_us` измеряет обработчик ввода, а не полную задержку кадра.

Порог фонового освобождения —64KiB текста, включая потомков больших состояний;
обычные маленькие документы и синхронное CLI-ядро сохраняют прежнее поведение.
Очередь передаёт уже выделенные объекты и не имеет жёсткого общего лимита памяти.
Счётчик logical bytes описывает длины Rope, а не уникальную занятую RAM. При
ошибке создания/отключении рабочего потока владение сохраняется, выполняется
учтённое синхронное освобождение. Линейные вставки, первое хеширование и полное
копирование снимков текста для K2 остаются отдельными задачами оптимизации.

### Снимки K2 без полного копирования на потоке интерфейса

Редактор отправляет в языковой сервис дешёвые неизменяемые снимки Rope, включая
несохранённые фоновые вкладки. Полный UTF-8 текст для LSP создаётся в фоновом пуле;
успешный результат разделяют активная и обновляемая K2-сессии. Более свежий снимок
отменяет устаревшее копирование. Перед подсказкой, F12 и семантической операцией
сервис дожидается именно последнего снимка, а новая модель проекта принимает
буферы только после настоящего импорта и проверки актуальности входных данных.
Версии, выделение, история и сохранённость документов при этом не меняются.

Большие Rope и кэшированные строки освобождаются через поток владения, а не на GPU.
При ошибке подготовки снимка последняя рабочая сессия сохраняется; ошибка не
кэшируется. Это полные снимки, не инкрементальные LSP-изменения: существующее
ограничение размера кадра16MiB остаётся. Общий объём удерживаемых буферов/очереди
не имеет жёсткого лимита; logical bytes не измеряет уникальную занятую RAM.

```sh
CARGO_BUILD_JOBS=2 bash scripts/check.sh
bash scripts/check-language-snapshots.sh /absolute/path/to/new-test-directory
```

Проверка поднимает настоящий K2 на Android-проекте без Gradle и APK, открывает
100000 строк и10 000 строк во второй вкладке. Она проверяет набор текста, Undo/Redo,
закрытие/открытие, несохранённую фоновую вкладку и F12 сразу после новой правки в
том же кадре ввода. Девять GPU-снимков и JSON фиксируют действительный переход к
объявлению, восстановление обоих буферов, актуальную диагностику и разные потоки
ввода/копирования. `foreground_capture_us` измеряет захват/публикацию снимка, не
полную задержку кадра, готовность K2 или скорость анализа произвольного проекта.

Семантические клавиши редактора обрабатываются по порядку событий: F12, подсказка,
поиск использований и переименование видят предшествующую вставку/IME commit и
новую позицию курсора даже в одном кадре. Глобальные сочетания остаются для других
панелей; фокусированный редактор не отправляет запрос дважды.
Сочетания отладки проверяют модификаторы точно: обычная F6 приостанавливает отладку,
а Shift+F6 доходит до переименования и не перехватывается обработчиком Debug.

### Horizontal long-line viewport

The native editor submits only visible glyph quads for clipped lines, while retaining
actual epaint font shaping, Unicode positions, colors, italic overhang and decorations.
A bounded weak block index also preserves exact native mouse-hit boundaries without
scanning an entire long row on every hit. Short fully visible lines use the stock path.
Run `bash scripts/check-viewport.sh /absolute/new/evidence-directory` for a real
500,000-character Unicode row: head/tail scrolling, mouse and keyboard selection,
Paste/Undo, IME and tab restoration, with CPU counters and genuine GPU captures.
These measurements distinguish update CPU from GPU-capture event roundtrips; neither
is a compositor presentation guarantee. Cold whole-line shaping and font-cache
teardown still perform linear work and remain future optimization targets.

### Background long-line shaping

Rows of at least16KiB now prepare exact epaint font layout, their immutable font atlas
and the ink/hit index on a lazy worker. The window keeps accepting input and shows a
preparation state until the current row is ready, rather than painting stale text or
inventing Unicode column widths. Font/DPI/composition/syntax changes fence obsolete
results. Private atlas meshes retain exact existing fonts, colors and IME styling.
Vertical navigation defers following input in order; deferred input never replays into
a replacement document or changed selection. Pending mouse gestures and subsequent
keyboard input replay in native event order; focus loss, Escape or tab activation
cancels deferred intent rather than applying a ghost edit.
`bash scripts/check-layout.sh /absolute/new/evidence-directory` verifies a real
2-million-character row, typing and a genuine GPU capture while shaping is active,
then exact selection/Undo/IME/tabs. Shaping remains linear CPU work; this moves it off
the window thread, not a universal latency/RAM bound or an incremental font engine.

### Ordered pointer gestures

Mouse and keyboard actions now share one ordered queue. Quick drag/release followed
by Paste or Copy is applied in that order, including while a long row is still being
shaped. Coordinates refer to the viewport the user saw; press modifiers/timing, shift
selection, word/line dragging and release survive deferral. Focus/tab/document/view
changes or Escape cancel pending intent. Only adjacent movement samples coalesce.
`bash scripts/check-pointer.sh /absolute/new/evidence-directory` verifies a real
2-million-character row and an actual same-frame native drag/release/Paste with15 GPU
captures. ASCII word navigation also avoids a tree lookup for every scalar, while
Unicode and CRLF edges retain the original grapheme algorithm.

### Focus-aware commands and Find handoff

Incoming editor clicks, text/clipboard/IME and editor commands dispatch in event
order before global shortcuts. Paste followed immediately by Save writes the new
snapshot; Save followed by later typing leaves that later edit dirty. Ctrl+Shift+S
saves all dirty tabs. Ctrl+F/H transfers subsequent input to the Find field even
while its Material3 spring panel is opening. A source click belongs only to the
viewport the user saw, not a Find control moving under that point during animation.
A baseline-only Save receipt no longer invalidates an already queued K2 definition
request by assigning unchanged source a new version.
`bash scripts/check-input-order.sh /absolute/new/evidence-directory` checks real
100k-line source/K2, Find input, incoming code click/Paste/Save/F12 in one native
frame, saved bytes, definition navigation and Undo+Save restoration with11 GPU
captures. This is event/clock correctness, not a universal latency guarantee.

### Терминал проекта

Новая вкладка **Terminal** открывает настоящую интерактивную оболочку через Linux
PTY в папке проекта. `Alt+F12` показывает/скрывает панель, `Ctrl+Shift+T` создаёт
ещё одну независимую сессию (до четырёх). Оболочка использует пользовательский
`SHELL`; работающий проект не запускает её до явного открытия терминала.

ANSI-цвета, Unicode, курсор, экранные программы и alternate screen обрабатывает
закреплённый `vt100 0.16.2`. Прокрутка: колесо или Shift+PageUp/PageDown, до2000 строк.
Выделение сохраняет показанный экран, а не меняющиеся под мышью новые данные.
Копирование выделения: кнопка Copy или Ctrl+Shift+C; Ctrl+C копирует выделенное,
а без выделения прерывает foreground-команду. Отдельная кнопка Interrupt всегда
посылает Ctrl+C. Вставка учитывает реальный bracketed-paste mode оболочки; IME
preedit показывается локально, UTF8-коммит отправляется единожды. Клавиши терминала
не сохраняют исходники и не ставят точки останова редактора.

Создание процессов, чтение/запись, VT-парсинг, resize и завершение работают вне
потока окна. Закрытие сессии завершает её оболочку и процессы того же SID через
проверенные pidfd; намеренно отделившиеся daemon-сессии не входят в эту гарантию.
После обычного закрытия окна IDE дожидается своих PTY workers вне GPU event loop.
Очереди и видимый grid ограничены (100x240), одно действие ввода — до64KiB;
переполнение показывает ошибку. Ограничен OSC-буфер; OSC52 не читает/не меняет
буфер обмена. Это не полная эмуляция xterm, graphics/mouse protocols или bidi и
не жёсткий предел общей RAM. Отрисовка IME показывает ограниченный префикс длинного
preedit; фактический допустимый коммит не обрезается.

`bash scripts/check-terminal.sh /absolute/new/evidence-directory` проверяет живое
окно, настоящие bash/readline и curses: controlling TTY/cwd, цветной CJK-вывод,
многострочную вставку до Enter, IME, выделение/scrollback, SIGWINCH/ArrowUp в alternate
screen, независимые сессии, SIGINT и закрытие фонового процесса с ignored HUP.
Исходники и Undo сохраняются, проверяются12 настоящих GPU-кадров.

Первичные API: [vt100](https://docs.rs/vt100/0.16.2/vt100/),
[Linux openpty](https://man7.org/linux/man-pages/man3/openpty.3.html),
[controlling terminal](https://man7.org/linux/man-pages/man2/TIOCSCTTY.2const.html).

### Ввод IME при переключении панелей

Композиция привязана к конкретному файлу и его активации, терминальной сессии либо
полю поиска. Переключение панели, файла, курсора или фокуса окна отменяет старую
композицию: её запоздавшее подтверждение не вставляется в новое место. Новый обычный
текст при этом сохраняется; новый сеанс IME возобновляется после сброса контекста.
Пустой preedit перед подтверждением и ввод после `Disabled` учитываются отдельно.

`Alt+F12` и следующий ввод обрабатываются по порядку, даже внутри одного кадра:
закрытие терминала возвращает следующий текст редактору. Интерфейс и прерываемые
Material 3 Expressive анимации остаются общими для редактора, поиска и терминала.

Проверка: `scripts/check-ime.sh` — настоящее GPU-окно, две PTY-сессии, переходы
редактор/поиск/терминал/файл, потеря фокуса, свежие подтверждения, Undo и неизменность
дисковых исходников. Это проверка нативного потока событий, а не автоматизация всех
системных IME-движков. В поиске preedit следует поведению egui TextEdit и меняет
черновик запроса; исходный Rope и терминальный ввод до подтверждения не меняются.

### Поиск по всему проекту

**Search** на боковой панели или **Ctrl+Shift+F** открывает **Find in Files**.
Введите запрос и нажмите Enter/Search. Совпадения сгруппированы по файлам, содержат
номер строки/столбца и контекст; **●** отмечает несохранённую вкладку. Клик по
совпадению открывает файл и выделяет точный текст. Из поля запроса **↓** переводит
фокус в результаты, **↑/↓** выбирают совпадение, **Enter** выполняет переход.
Есть регистр, Unicode whole-word и регулярные выражения.

Поиск читает несохранённые активную и фоновые вкладки вместо старых дисковых копий;
сам ничего не сохраняет и не меняет Undo. Запрос и изменения открытых буферов
обновляются с debounce. Stop отменяет текущую/отложенную работу. При ошибке или
устаревании предыдущие результаты остаются видимыми, но переходы отключены.
Search повторно читает диск; изменившийся после поиска закрытый файл проверяется
перед открытием, поэтому старое совпадение не переносит курсор в другой текст.

Проверяются UTF-8 текстовые файлы любых расширений и скрытые текстовые файлы.
Генерируемые `.git/.gradle/.wex/build/target/node_modules/.verification` исключены;
символические ссылки, спецфайлы, бинарные/невалидные UTF-8 файлы пропускаются.
Лимиты показаны в подсказке статистики: 8 МиБ/файл, 256 МиБ чтения и снимков,
20 000 файлов, 100 000 записей обхода, 5000 совпадений. Частичный результат отмечен
явно. Правила `.gitignore` пока не интерпретируются; изменения закрытых файлов
обновляются кнопкой Search, а не непрерывным фоновым сканированием.

Проверка: `scripts/check-project-search.sh OUTPUT [--serial emulator-5580]` —
реальные виджеты и GPU-окно, несохранённые вкладки, мышь/клавиатура, Unicode,
Undo/Redo, regex/ошибка/отмена, внешний процесс записи и восстановление исходников.

### Unified WEX icons

The native shell uses Material Symbols Rounded imported offline from WEXdesktop,
with matching colored file badges in the project tree, document tabs and Find in
Files groups. Android manifests, Kotlin/Java/XML, native configuration, libraries,
packages and other file types no longer share a generic code icon. Folder glyphs
reflect their expanded state; unsaved/conflict markers remain independent.
Only two small static icon subsets are embedded; their named font families do not
change source font fallbacks. No JVM or runtime icon download is involved.
Origin, retained codepoints and hashes: `assets/icons/manifest.json`; font license
and modification notice ship in the Linux package. Regeneration is developer-only:
`python3 scripts/import-wex-symbols.py --source /path/to/WEXdesktop`.

### Замена по всему проекту с общим Undo

В **Find in Files** (`Ctrl+Shift+F`) задайте запрос и параметры поиска, нажмите
**Replace…**, введите замену и нажмите **Preview**. В предпросмотре показаны файлы,
исходные и новые фрагменты, строка/колонка и метка несохранённого источника.
**Apply reviewed** применяет только этот проверенный снимок. Изменение запроса,
замены, буфера или дискового файла требует нового предпросмотра; ошибка одного
файла отклоняет всю операцию. **Cancel / Escape** не меняет буферы.

Источником служат все открытые вкладки, включая несохранённые неактивные.
Совпадения в закрытых файлах открываются в несохранённых вкладках. Диск меняется
только после явного Save — Apply не выполняет скрытое сохранение.
**Undo all / Redo all** отменяет/повторяет всю замену. `Ctrl+Z`, `Ctrl+Y` или
`Ctrl+Shift+Z` в затронутой вкладке на границе этой операции тоже использует общую
историю. Последующие локальные правки сначала отменяются обычным Undo. Если
другая затронутая вкладка закрыта или изменилась, общая отмена отклоняется целиком;
исходная история и уже сохранённая дисковая база сохраняются.

Литеральная замена не интерпретирует `$` и `\n`. Режим Regex поддерживает `$1`,
`${name}`, `$$`, `\n`, `\r`, `\t` и `\\`. Пустая замена удаляет совпадения.
Пределы: 1,000 изменяемых файлов, 64 MiB входа+выхода плана, 8 MiB результата
на файл, 16 KiB шаблона (поле UI — 4,096 символов), 5,000 совпадений поиска.
Частичный поиск не допускает Apply. Фрагменты предпросмотра ограничены 240 символами,
а не выдаются за полный построчный diff. История хранит до 16 общих операций;
это не общий жёсткий лимит RSS. Проверка: `scripts/check-project-replace.sh OUTPUT emulator-5580`.

В окне Replace in Files запрос и Aa/Whole word/Regex можно менять сразу, без
закрытия диалога. Поля имеют постоянные подписи. Enter в поле вызывает Preview;
Ctrl+Enter применяет только актуальный непустой предпросмотр. IME candidate Enter
не вызывает ни Preview, ни Apply. Устаревший review остаётся видимым с пометкой,
но отключён для применения. Preview можно обновить во время подготовки; изменение
ввода отменяет старый запрос, а не буферы. Cancel возвращает клавиатурный фокус.
Кнопки остаются доступны при760×560 и развёрнутых Details длинной ошибки.
Дополнительная проверка: `scripts/check-project-replace-ux.sh OUTPUT emulator-5580`.

### Local Git

Open **Git changes** in Project, or **Ctrl+Shift+G**. Review Working tree and Staged
file diffs, Stage/Unstage a selected file, then explicitly **Commit staged** with a
message. Git uses disk/index bytes, not unsaved editor tabs: Save dirty files and
Refresh before staging them. There is no implicit Save, repository initialization,
stage-all, push or discard. The installed Git handles normal hooks/signing. Failed
operations keep the message and offer Details/Refresh; use external Git for remote,
branch, submodule/conflict resolution and partial-hunk operations.

In Git, unsaved-only tabs appear in Working tree as editor buffers, not Git changes.
**Save selected** (Ctrl+S in the files list) explicitly saves only that row's affected
editor tabs, then refreshes its disk diff; Stage is still a separate action. F6 switches
files/message; arrows/Home/End/Page navigate; Enter or **Open file** returns to the
editor with dirty buffers and Undo kept. External changes refresh every3s while the
review is open; message focus and selected paths survive. Errors offer Details/Refresh.

Explicit Save/Refresh keeps the selected file visible even when its row moves from
unsaved-only tabs into Git status. Idle outside checks retain the current scroll.

For this desktop workflow alone, use `scripts/check-git-flow.sh OUTPUT --desktop-only`.
This opens the real native GPU editor without booting an Android emulator; it does
not verify APK installation, Run or Android Debug. The original explicit
`OUTPUT emulator-5580` mode remains available. Git/Replace native harnesses clean up
their own background services on exit or failure, without closing existing apps.
