from iddb.logging import logger

class GlobalHandler:
    GDB_SESSION_CLEAN_HANDLE = None
    DDB_EXIT_HANDLE = None

    @staticmethod
    def remove_session(sid: int):
        logger.debug(f"Removing session: {sid}")
        if GlobalHandler.GDB_SESSION_CLEAN_HANDLE:
            GlobalHandler.GDB_SESSION_CLEAN_HANDLE(sid)

    @staticmethod
    def exit_ddb():
        if GlobalHandler.DDB_EXIT_HANDLE:
            GlobalHandler.DDB_EXIT_HANDLE()