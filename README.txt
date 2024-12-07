# CSCI651 Project C Final Submissoin


## Overview

This project folder contains all the source code and benchmark for this course project. Since my course project built something on top of another bigger system, this folder contains more than my course project covers.

It also has the full implementation of the prototype distributed debugger.

## Structure

The code I programmed for this course project are scattered across the core interactive debugger implementation, and the gdb extension script which is used to adjust the faketime. The faketime library is included as a standalone library in folder `libfaketime`.

The core implementation of the interactive distribued debugger is in folder `ddb`. The gdb extension scripts is located in folder `ddb/iddb/gdb_ext/runtime-gdb-grpc.py`.

The benchmark and testing scripts and results are in the folder `faketimetest` and `exp`.

## Weekly Meeting
Link: https://docs.google.com/document/d/15VpjCTdpt9iaSmEq5FGj3eE6408AVcLtIHY6URXFIa8/edit?tab=t.0
