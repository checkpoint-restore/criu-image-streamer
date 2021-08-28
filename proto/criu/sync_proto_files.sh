#!/bin/bash
set -eux

sed --version > /dev/null # This commeand fails on BSD, and we don't want the BSD version of sed

if [ $# != 1 ]; then
  echo "Usage: $0 /path/to/criu/project"
  exit 1
fi

CRIU_SRC=$1
cp -a "$CRIU_SRC"/images/LICENSE .
cp -a "$CRIU_SRC"/images/*.proto .
sed -i -E "s/^(syntax =.*)$/\1\npackage criu;/g" ./*.proto
# This is to avoid a conflict with "message criu_opts" in rpc.proto
sed -i -E "s/CRIU_Opts/CRIU_Opts_pb/g" opts.proto
chmod 644 ./*.proto
