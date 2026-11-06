Wex Studio — native Linux x86_64 package

Run ./wexstudio for the projectless welcome screen, or ./wexstudio /absolute/project.
A wex.json file argument opens its parent project. Run ./bin/wex doctor --require-support
for a read-only support-integrity and external-tool availability report.

The uncompressed native frontend executable has a strict 20,000,000-byte package gate.
manifest.json separately reports CLI, support, combined uncompressed and archive sizes.
Android SDK/JDK, Kotlin LSP/compiler caches and local Layoutlib remain external tools;
they are neither hidden inside the frontend size nor bundled in this archive.

Immutable support assets live in share/wex. Mutable tools default to
$XDG_DATA_HOME/wex/tools (or $HOME/.local/share/wex/tools). WEX_SUPPORT_HOME and
WEX_TOOLS_HOME override those roots, resolved once against the caller's launch cwd.
For an existing development checkout, WEX_TOOLS_HOME=/absolute/checkout/.wex reuses
its prepared tools. No normal welcome launch performs source discovery, K2/device
startup, dependency resolve, provisioning or downloads.

Explicit preparation: ./bin/wex prepare-lsp --offline verifies/reuses a cached pinned
Kotlin LSP archive. Without --offline that explicit action may fetch the pinned LSP.
Native project dependencies/compiler tools resolve through Wex, never Gradle/Maven.
Debug and Preview use the bundled helper sources and configured external tool data.

Optional menu entry: python3 ./install-desktop.py
Or choose a directory explicitly: python3 ./install-desktop.py --applications /path/to/apps
The installer creates one new shortcut and leaves associations/autostart unchanged.

Release binaries use size optimization, thin LTO, symbol stripping and panic=abort.
Unexpected Rust panics end the process; ordinary SDK/config/protocol errors use Results.
GUI/shared graphics libraries and optional system CJK fonts are supplied by the OS.

Java formatting uses an external SHA256-pinned google-java-format1.37.0 tool JAR.
The IDE can prepare it on first Java formatting; `bin/wex prepare-java-formatter`
prepares it explicitly (`--offline` verifies an existing copy only). JDK21+ is required;
WEX_FORMAT_JAVA_HOME explicitly overrides JAVA_HOME for this isolated helper. Relative
roots bind to the launch directory. Style is AOSP/4-space; imports are not optimized.
This is a formatter, not Java type analysis. Its tool JAR is not in the frontend/APK.
