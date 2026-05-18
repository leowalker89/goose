# Examples

All three examples do the same thing: build an `Agent`, configure it with a provider, and send the prompt `"ping apple.com"`. The agent will call the `developer__shell` tool (or equivalent) and stream the output back.

## Prerequisites

```bash
# 1. From the repo root, build the cdylib and bindgen binary:
cargo build -p goose-uniffi

# 2. Configure a goose provider (only needs to be done once on your machine):
goose configure

# 3. Generate bindings for whichever languages you want to use:
LIB=./target/debug/libgoose_uniffi.dylib   # .so on Linux, .dll on Windows
./target/debug/goose-uniffi-bindgen generate --library $LIB --language python --out-dir ./sdk/generated
./target/debug/goose-uniffi-bindgen generate --library $LIB --language kotlin --out-dir ./sdk/generated
```

All examples assume `libgoose_uniffi.{dylib,so}` is on the dynamic-library load path. The snippets below set that automatically.

---

## Python

**File:** `ping_apple.py`

```bash
DYLD_LIBRARY_PATH=./target/debug \
LD_LIBRARY_PATH=./target/debug \
python3 sdk/examples/ping_apple.py
```

Requires Python 3.8+. No pip packages needed — `ctypes` ships with the stdlib.

---

## Kotlin

**File:** `PingApple.kt`

Compile and run via `kotlinc` + `java`:

```bash
# Compile (jna is needed by the uniffi-generated kotlin runtime)
JNA_JAR=$(find ~/.gradle /opt/homebrew /usr/local -name 'jna-5.*.jar' 2>/dev/null | head -1)
# If you don't have jna locally, grab it:
#   curl -L -o jna.jar https://repo1.maven.org/maven2/net/java/dev/jna/jna/5.14.0/jna-5.14.0.jar
#   JNA_JAR=./jna.jar

kotlinc -cp "$JNA_JAR" \
  sdk/generated/uniffi/goose_uniffi/goose_uniffi.kt \
  sdk/examples/PingApple.kt \
  -include-runtime -d ping_apple.jar

DYLD_LIBRARY_PATH=./target/debug \
LD_LIBRARY_PATH=./target/debug \
java -cp "ping_apple.jar:$JNA_JAR" examples.PingAppleKt
```

Requires `kotlinc` (`brew install kotlin`) and a JDK.

---

## Notes

- The first run creates a session under `~/.local/share/goose/sessions/`. Subsequent runs make new sessions; this PoC doesn't expose resume.
- Provider/model come from `~/.config/goose/config.yaml` by default. Override per-run with `GOOSE_PROVIDER=openai GOOSE_MODEL=gpt-4o just python`.
- Each example loads the `developer` builtin extension so the agent can run shell commands like `ping`.

## TypeScript

Not supported by this SDK. UniFFI core has no TypeScript backend, and the React-Native-only `uniffi-bindgen-react-native` doesn't fit a plain-Node use case. For TS, use the existing ACP path in [`../../crates/goose-sdk`](../../crates/goose-sdk) and [`../../ui/sdk`](../../ui/sdk) instead.
