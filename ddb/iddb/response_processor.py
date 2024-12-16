from threading import Lock, Thread
from queue import Queue
from iddb.cmd_tracker import CmdTracker
from iddb.event_loop import GlobalRunningLoop
from iddb.global_handler import GlobalHandler
from iddb.state_manager import StateManager, ThreadStatus
from iddb.data_struct import SessionResponse
from iddb.response_transformer import GenericStopAsyncRecordTransformer, ResponseTransformer, RunningAsyncRecordTransformer, StopAsyncRecordTransformer, ThreadCreatedNotifTransformer, ThreadExitedNotifTransformer, ThreadGroupNotifTransformer
from iddb.logging import logger

import asyncio

class ResponseProcessor:
    _instance: "ResponseProcessor" = None

    def __init__(self) -> None:
        self.queue: asyncio.Queue[SessionResponse] = asyncio.Queue()
        self.state_manager = StateManager.inst()
        loop: asyncio.AbstractEventLoop = GlobalRunningLoop().get_loop()
        self.process_task = asyncio.run_coroutine_threadsafe(self.process(), loop)

    @staticmethod
    def inst() -> "ResponseProcessor":
        if ResponseProcessor._instance:
            return ResponseProcessor._instance
        ResponseProcessor._instance = ResponseProcessor()
        return ResponseProcessor._instance

    async def put(self, response: SessionResponse):
        await self.queue.put(response)

    async def process(self):
        try:
            while True:
                resp = await self.queue.get()
                resp_type = resp.response["type"]

                if resp_type == "notify":
                    await self.handle_notify(resp)

                if resp_type == "result":
                    await self.handle_result(resp)
                
                self.queue.task_done()
        except Exception as e:
            logger.exception(f"Error in response processor: {e}")

    async def handle_result(self, response: SessionResponse):
        await CmdTracker.inst().recv_response(response)

    async def handle_notify(self, response: SessionResponse):
        sid = response.sid
        resp_msg = response.response["message"]
        resp_payload = response.response["payload"]

        if resp_msg == "thread-created":
            tgid = str(resp_payload["group-id"])
            gtid, giid = self.state_manager.create_thread(
                sid, int(resp_payload["id"]), tgid)
            ResponseTransformer.output(
                response, ThreadCreatedNotifTransformer(gtid, giid, sid))
        elif resp_msg == "thread-exited":
            gtid=self.state_manager.sidtid_to_gtid[(sid,int(resp_payload["id"]))]
            self.state_manager.remove_thread(sid, int(resp_payload["id"]))
            tgid = str(resp_payload["group-id"])
            giid=self.state_manager.sidtgid_to_giid[(sid, tgid)]
            ResponseTransformer.output(
                response, ThreadExitedNotifTransformer(gtid, giid,sid))
        elif resp_msg == "running":
            thread_id = resp_payload["thread-id"]
            if thread_id == "all":
                self.state_manager.update_all_thread_status(
                    sid, ThreadStatus.RUNNING)
                ResponseTransformer.output(
                    response, RunningAsyncRecordTransformer(all_running=True))
            else:
                thread_id = int(thread_id)
                self.state_manager.update_thread_status(
                    sid, thread_id, ThreadStatus.RUNNING)
                ResponseTransformer.output(
                    response, RunningAsyncRecordTransformer(all_running=False))
        elif resp_msg == "stopped":
            if "reason" in resp_payload and "exit" in resp_payload["reason"]:
                GlobalHandler.remove_session(sid)
                # self.state_manager.remove_session(sid)
                return

            if "thread-id" in resp_payload:
                thread_id = resp_payload["thread-id"]
                if thread_id == "all":
                    self.state_manager.update_all_thread_status(
                        sid, ThreadStatus.STOPPED)
                else:
                    thread_id = int(thread_id)
                    self.state_manager.update_thread_status(
                        sid, thread_id, ThreadStatus.STOPPED)
                    # Here, we assume it runs in all-stop mode.
                    # Therefore, when a thread hits a breakpoint,
                    # all threads stops and the currently stopped thread
                    # as the current selected thread automatically.
                    if resp_payload.get("reason","none") == "breakpoint-hit":
                    # Here, we assume it runs in all-stop mode. 
                    # Therefore, when a thread hits a breakpoint, 
                    # all threads stops and the currently stopped thread 
                    # as the current selected thread automatically.
                        self.state_manager.set_current_tid(sid, thread_id)
                        self.state_manager.set_current_gthread(self.state_manager.get_gtid(sid, thread_id))
                stopped_threads = resp_payload["stopped-threads"]
                if stopped_threads == "all":
                    self.state_manager.update_all_thread_status(
                        sid, ThreadStatus.STOPPED)
                else:
                    # In non-stop modes, we need to handle a list of threads as they may stop at different times.
                    for t in stopped_threads:
                        tid = int(t)
                        self.state_manager.update_thread_status(
                            sid, tid, ThreadStatus.STOPPED
                        )
                ResponseTransformer.output(response, StopAsyncRecordTransformer())
            else:
                ResponseTransformer.output(response, GenericStopAsyncRecordTransformer())
        elif resp_msg == "thread-group-added":
            tgid = str(resp_payload['id'])
            gtgid = self.state_manager.add_thread_group(sid, tgid)
            ResponseTransformer.output(
                response, ThreadGroupNotifTransformer(gtgid))
        elif resp_msg == "thread-group-removed":
            tgid = str(resp_payload['id'])
            gtgid = self.state_manager.remove_thread_group(sid, tgid)
            ResponseTransformer.output(
                response, ThreadGroupNotifTransformer(gtgid))
        elif resp_msg == "thread-group-started":
            tgid = str(resp_payload['id'])
            pid = int(resp_payload["pid"])
            gtgid = self.state_manager.start_thread_group(sid, tgid, pid)
            ResponseTransformer.output(
                response, ThreadGroupNotifTransformer(gtgid))
        elif resp_msg == "thread-group-exited":
            tgid = str(resp_payload['id'])
            gtgid = self.state_manager.exit_thread_group(sid, tgid)
            ResponseTransformer.output(
                response, ThreadGroupNotifTransformer(gtgid))
        else:
            logger.debug(f"Ignoring this notify record for now: {response}")

# class ResponseProcessor:
#     _instance: "ResponseProcessor" = None
#     _lock = Lock()

#     def __init__(self) -> None:
#         self.queue: Queue[SessionResponse] = Queue(maxsize=0)
#         self.state_manager = StateManager.inst()
#         self.process_handle = Thread(
#             target=self.process, args=()
#         )
#         self.process_handle.start()

#     @staticmethod
#     def inst() -> "ResponseProcessor":
#         with ResponseProcessor._lock:
#             if ResponseProcessor._instance:
#                 return ResponseProcessor._instance
#             ResponseProcessor._instance = ResponseProcessor()
#             return ResponseProcessor._instance

#     def put(self, response: SessionResponse):
#         self.queue.put_nowait(response)

#     def process(self):
#         try:
#             while True:
#                 resp = self.queue.get()
#                 resp_type = resp.response["type"]

#                 # Special handling for different types of response
#                 # mi_print(resp.response, resp.meta)

#                 if resp_type == "notify":
#                     self.handle_notify(resp)

#                 if resp_type == "result":
#                     self.handle_result(resp)
#         except Exception as e:
#             logger.exception(f"Error in response processor: {e}")

#     def handle_result(self, response: SessionResponse):
#         CmdTracker.inst().recv_response(response)

#     def handle_notify(self, response: SessionResponse):
#         sid = response.sid
#         resp_msg = response.response["message"]
#         resp_payload = response.response["payload"]

#         if resp_msg == "thread-created":
#             tgid = str(resp_payload["group-id"])
#             gtid, giid = self.state_manager.create_thread(
#                 sid, int(resp_payload["id"]), tgid)
#             ResponseTransformer.output(
#                 response, ThreadCreatedNotifTransformer(gtid, giid,sid))
#         elif resp_msg == "thread-exited":
#             gtid=self.state_manager.sidtid_to_gtid[(sid,int(resp_payload["id"]))]
#             self.state_manager.remove_thread(sid, int(resp_payload["id"]))
#             tgid = str(resp_payload["group-id"])
#             giid=self.state_manager.sidtgid_to_giid[(sid, tgid)]
#             ResponseTransformer.output(
#                 response, ThreadExitedNotifTransformer(gtid, giid,sid))
#         elif resp_msg == "running":
#             thread_id = resp_payload["thread-id"]
#             if thread_id == "all":
#                 self.state_manager.update_all_thread_status(
#                     sid, ThreadStatus.RUNNING)
#                 ResponseTransformer.output(
#                     response, RunningAsyncRecordTransformer(all_running=True))
#             else:
#                 thread_id = int(thread_id)
#                 self.state_manager.update_thread_status(
#                     sid, thread_id, ThreadStatus.RUNNING)
#                 ResponseTransformer.output(
#                     response, RunningAsyncRecordTransformer(all_running=False))
#         elif resp_msg == "stopped":
#             if "reason" in resp_payload and "exit" in resp_payload["reason"]:
#                 GlobalHandler.remove_session(sid)
#                 # self.state_manager.remove_session(sid)
#                 return

#             if "thread-id" in resp_payload:
#                 thread_id = resp_payload["thread-id"]
#                 if thread_id == "all":
#                     self.state_manager.update_all_thread_status(
#                         sid, ThreadStatus.STOPPED)
#                 else:
#                     thread_id = int(thread_id)
#                     self.state_manager.update_thread_status(
#                         sid, thread_id, ThreadStatus.STOPPED)
#                     # Here, we assume it runs in all-stop mode.
#                     # Therefore, when a thread hits a breakpoint,
#                     # all threads stops and the currently stopped thread
#                     # as the current selected thread automatically.
#                     if resp_payload.get("reason","none") == "breakpoint-hit":
#                     # Here, we assume it runs in all-stop mode. 
#                     # Therefore, when a thread hits a breakpoint, 
#                     # all threads stops and the currently stopped thread 
#                     # as the current selected thread automatically.
#                         self.state_manager.set_current_tid(sid, thread_id)
#                         self.state_manager.set_current_gthread(self.state_manager.get_gtid(sid, thread_id))
#                 stopped_threads = resp_payload["stopped-threads"]
#                 if stopped_threads == "all":
#                     self.state_manager.update_all_thread_status(
#                         sid, ThreadStatus.STOPPED)
#                 else:
#                     # In non-stop modes, we need to handle a list of threads as they may stop at different times.
#                     for t in stopped_threads:
#                         tid = int(t)
#                         self.state_manager.update_thread_status(
#                             sid, tid, ThreadStatus.STOPPED
#                         )
#                 ResponseTransformer.output(response, StopAsyncRecordTransformer())
#             else:
#                 ResponseTransformer.output(response, GenericStopAsyncRecordTransformer())
#         elif resp_msg == "thread-group-added":
#             tgid = str(resp_payload['id'])
#             gtgid = self.state_manager.add_thread_group(sid, tgid)
#             ResponseTransformer.output(
#                 response, ThreadGroupNotifTransformer(gtgid))
#         elif resp_msg == "thread-group-removed":
#             tgid = str(resp_payload['id'])
#             gtgid = self.state_manager.remove_thread_group(sid, tgid)
#             ResponseTransformer.output(
#                 response, ThreadGroupNotifTransformer(gtgid))
#         elif resp_msg == "thread-group-started":
#             tgid = str(resp_payload['id'])
#             pid = int(resp_payload["pid"])
#             gtgid = self.state_manager.start_thread_group(sid, tgid, pid)
#             ResponseTransformer.output(
#                 response, ThreadGroupNotifTransformer(gtgid))
#         elif resp_msg == "thread-group-exited":
#             tgid = str(resp_payload['id'])
#             gtgid = self.state_manager.exit_thread_group(sid, tgid)
#             ResponseTransformer.output(
#                 response, ThreadGroupNotifTransformer(gtgid))
#         else:
#             logger.debug(f"Ignoring this notify record for now: {response}")

    # def handle_notify_thread_group(self, response: SessionResponse):
    #     sid = response.sid
    #     resp_msg = response.response["message"]
    #     resp_payload = response.response["payload"]

    #     if resp_msg == "thread-group-added":
    #         tgid = str(resp_payload['id'])
    #         self.state_manager.add_thread_group(sid, tgid)

    #     if resp_msg == "thread-group-started":
    #         tgid = str(resp_payload['id'])
    #         pid = int(resp_payload["pid"])
    #         self.state_manager.start_thread_group(sid, tgid, pid)

    #     if resp_msg == "thread-group-exited":
    #         tgid = str(resp_payload['id'])
    #         self.state_manager.exit_thread_group(sid, tgid)


# Eager instantiation
# _ = ResponseProcessor.inst()
