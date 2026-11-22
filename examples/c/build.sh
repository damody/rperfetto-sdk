#!/bin/sh
# Builds the C example against the static library.
#   examples/c/build.sh                 host (release)
#   TARGET=aarch64-linux-android examples/c/build.sh   Android (needs NDK)
set -e
cd "$(dirname "$0")/../.."
TARGET=${TARGET:-}
OUT=examples/c/build
mkdir -p "$OUT"
if [ -z "$TARGET" ]; then
  cargo build --release --lib
  LIB=target/release/librperfetto_sdk.a
  cc -O2 -Wall -Wextra -Iinclude examples/c/c_demo.c $LIB -lpthread -ldl -lm -o "$OUT/c_demo"
  # Function MIPS demo: workload instrumented, hooks not.
  cc -O2 -Wall -Wextra -Iinclude -finstrument-functions examples/c/fn_mips_demo.c \
    examples/c/instrument_hooks.c $LIB -rdynamic -lpthread -ldl -lm -o "$OUT/fn_mips_demo"
else
  NDK=${ANDROID_NDK_HOME:-$HOME/.android-ndk/android-ndk-r29}
  API=${ANDROID_API:-29}
  CC="$NDK/toolchains/llvm/prebuilt/linux-x86_64/bin/${TARGET}${API}-clang"
  cargo build --release --lib --target "$TARGET"
  LIB="target/$TARGET/release/librperfetto_sdk.a"
  "$CC" -O2 -Wall -Wextra -Iinclude examples/c/c_demo.c "$LIB" -ldl -lm -o "$OUT/c_demo-$TARGET"
  "$CC" -O2 -Wall -Wextra -Iinclude -finstrument-functions examples/c/fn_mips_demo.c \
    examples/c/instrument_hooks.c "$LIB" -Wl,--export-dynamic -ldl -lm -o "$OUT/fn_mips_demo-$TARGET"
fi
echo "built $OUT"
