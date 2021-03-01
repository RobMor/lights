#!/bin/bash

set -eux

TARGET_HOST=pi@lights
TARGET_PATH=/home/pi/lights
TARGET_ARCH=arm-unknown-linux-gnueabihf
SOURCE_PATH=./target/${TARGET_ARCH}/release/lights

cargo build --release --target=${TARGET_ARCH}
rsync ${SOURCE_PATH} ${TARGET_HOST}:${TARGET_PATH}
ssh -t ${TARGET_HOST} sudo ${TARGET_PATH}