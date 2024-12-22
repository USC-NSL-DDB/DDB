import asyncio
import threading

MAIN_LOOP: asyncio.AbstractEventLoop = None
DBG_MANAGER: "GdbManager" = None
TERMINATED = False

# Current used for the clean up logic
# Due to event loop implementation, clean logic can be triggered
# from multiple places and current running clean code can be swapped
# to another clean up code. This lock is used to ensure only one clean.
G_LOCK = threading.Lock()


