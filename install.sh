#!/bin/sh

CONFIG_DIR=~/.config/br/
DATA_DIR=~/.local/share/br/

mkdir -p $CONFIG_DIR $DATA_DIR

if [ ! -f $CONFIG_DIR/config.toml ]; then
  cp config.example.toml $CONFIG_DIR/config.toml
  echo "Copied example config to $CONFIG_DIR"
fi

cargo install --path .
