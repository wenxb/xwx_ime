#!/usr/bin/env bash
# Build rime_oracle against the system librime (1.10.0).
set -euo pipefail
cd "$(dirname "$0")"
g++ -std=c++17 -O2 -Wall -o rime_oracle rime_oracle.cc \
    /usr/lib/x86_64-linux-gnu/librime.so.1
