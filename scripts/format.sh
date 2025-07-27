#!/bin/bash

cargo clippy --fix --allow-dirty --all-targets --all-features -- -D warnings -D clippy::all && cargo fmt --all