#!/usr/bin/env bash

set -e

find ./{supervisor,system,tau}/src -name "*.rs" -type f -print | xargs wc -l

# find build -type f -print | xargs wc -l
