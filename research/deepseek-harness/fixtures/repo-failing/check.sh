#!/bin/sh
# A gate that fails the same way every time.
echo "check: upstream registry not ready (E_NOT_READY)" >&2
exit 1
