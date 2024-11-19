# distributed-debugger

## Validation Rules
1. `caller_ctx` in remote backtrace extension fields must ONLY use the predefined register aliases:
   - "pc" for Program Counter
   - "sp" for Stack Pointer
   - "fp" for Frame Pointer
   - "lr" for Link Register
2. No other register names are permitted

