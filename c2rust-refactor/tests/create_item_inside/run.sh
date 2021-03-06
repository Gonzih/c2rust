#!/bin/sh

# work around System Integrity Protection on macOS
if [ `uname` = 'Darwin' ]; then
    export LD_LIBRARY_PATH=$not_LD_LIBRARY_PATH
fi

$refactor \
    select dest 'crate; desc(mod && name("test"));' \; \
    create_item 'fn new() {}' inside dest \
    -- old.rs $rustflags
