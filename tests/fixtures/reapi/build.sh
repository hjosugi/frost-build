#!/bin/sh
set -eu
mkdir -p result/nested
tr '[:lower:]' '[:upper:]' < input.txt > result/nested/output.txt
