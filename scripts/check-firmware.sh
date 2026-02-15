#!/usr/bin/env bash
set -euo pipefail

cp -n firmware/src/secrets.rs.example firmware/src/secrets.rs
(
  cd firmware
  cargo check
)
