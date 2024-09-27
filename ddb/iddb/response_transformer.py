from iddb.data_struct import SessionResponse
from typing import Any, List, Union
from iddb.mi_formatter import MIFormatter
from iddb.state_manager import StateManager
from iddb import utils
from iddb.logging import logger

class TransformerBase:
    def __init__(self) -> None:
        # self.responses = responses
        pass
        
    def transform(self, responses: List[SessionResponse]) -> dict:
        raise NotImplementedError

    def format(self, responses: List[SessionResponse]) -> str:
        return self.transform(responses) 

    # def transform_stdout(self, responses: List)

class NullTransformer(TransformerBase):
    def transform(self, responses: List[SessionResponse]) -> dict:
        return ""

    def format(self, responses: List[SessionResponse]) -> str:
        return self.transform(responses) 
''' Just a dummy transformer
'''
class PlainTransformer(TransformerBase):
    def __init__(self) -> None:
        super().__init__()
        # pass

    def transform(self, responses: List[SessionResponse]) -> dict:
        out_dict = {
            "data": [ f"{res.meta} [msg: {res.response['message']}] \n{res.response['payload']}" for res in responses ]
        }
        return out_dict
        # out_str = "\n".join([ f"{res.meta} [msg: {res.response['message']}] \n{res.response['payload']}" for res in responses ])
        # out_str = utils.wrap_grouped_message(out_str)
        # return out_str

    def format(self, responses: List[SessionResponse]) -> str:
        data = self.transform(responses)
        #out_str = "\n".join(data["data"])
        #out_str = utils.wrap_grouped_message(out_str)
        if responses[0].payload and responses[0].token is not None:
            out_str = MIFormatter.format("^", responses[0].msg, responses[0].payload, responses[0].token)
        elif responses[0].msg is not None:
            out_str =  MIFormatter.format_message("^", responses[0].msg, responses[0].token)
        else:
            out_str = "\n".join(data["data"])
        return out_str

''' Handling `-thread-info` response
'''
class ThreadInfoTransformer(TransformerBase):
    def __init__(self) -> None:
        super().__init__()
        
    def transform(self, responses: List[SessionResponse]) -> dict:
        all_threads_info = []
        for res in responses:
            if res.payload and ("threads" in res.payload):
                threads = res.payload["threads"]
                sid = res.sid
                for t in threads:
                    tid = int(t["id"])
                    t["id"] = str(StateManager.inst().get_gtid(sid, tid))
                    all_threads_info.append(t)

        all_threads_info = sorted(all_threads_info, key=lambda x: x["id"])

        # TODO: handle current-thread-id
        out_dict = { 
            "threads": all_threads_info,
            "current-thread-id": StateManager.inst().get_current_gthread()
        }
        return out_dict

    def format(self, responses: List[SessionResponse]) -> str:
        data = self.transform(responses)
        response=responses[0]
        out_str = MIFormatter.format("^", "done", data, response.token)
        return out_str

''' Handling `-list-thread-groups` response
'''
class ProcessInfoTransformer(TransformerBase):
    def __init__(self) -> None:
        super().__init__()

    def transform(self, responses: List[SessionResponse]) -> dict:
        all_process_info = []
        for res in responses:
            if res.payload and ("groups" in res.payload):
                processes = res.payload["groups"]
                sid = res.sid
                for p in processes:
                    local_tgid = str(p["id"])
                    p["id"] = f"i{StateManager.inst().get_giid(sid, local_tgid)}"
                    all_process_info.append(p) 

        all_process_info = sorted(all_process_info, key=lambda x: x["id"])
        out_dict = { "groups": all_process_info }
        return out_dict

    def format(self, responses: List[SessionResponse]) -> str:
        data = self.transform(responses)
        out_str = MIFormatter.format("^", "done", data, None)
        return out_str

''' Handling `info inferiors` response
'''
class ProcessReadableTransformer(TransformerBase):
    def __init__(self) -> None:
        super().__init__()

    def transform(self, responses: List[SessionResponse]) -> dict:
        pinfo = ProcessInfoTransformer().transform(responses)
        out_dict = {
            "groups": [ 
                { 
                    "id": int(p["id"][1:]), 
                    "desc": f"{p['type']} {p['pid']}",
                    "exec": p.get("executable","")
                } 
                for p in pinfo["groups"] 
            ]
        }
        return out_dict

    def format(self, responses: List[SessionResponse]) -> str:
        data = self.transform(responses)
        out_str = "Num\tDescription\tExecutable\n"
        # out_str = "\n".join(data["groups"])
        for g in data["groups"]:
            out_str += f"{g['id']}\t{g['desc']}\t{g['exec']}\n"
        # out_str = utils.wrap_grouped_message(out_str)
        return out_str

''' Handling `info threads` response
'''
class ThreadInfoReadableTransformer(TransformerBase):
    def __init__(self) -> None:
        super().__init__()
        self.tinfo_transformer = ThreadInfoTransformer()

    def transform(self, responses: List[SessionResponse]) -> dict:
        tinfo = self.tinfo_transformer.transform(responses)
        # out_dict = {
        #     "thread": [ f"thread {t['id']} {t['target-id']} {t['frame']['addr']}" for t in tinfo["thread"] ],
        #     "current-thread-id": tinfo["current-thread-id"]
        # }
        return tinfo 

    def format(self, responses: List[SessionResponse]) -> str:
        data = self.transform(responses)
        out_str = "\tId\tTarget Id\tFrame\n"
        out_entries = []
        for t in data["threads"]:
            # func_args_str = f"({[a['name'] for a in t['frame']['args']]})"
            # full_func = f"{t['frame']['func']} at {t['frame']['addr']}"
            tid = StateManager.inst().get_readable_tid_by_gtid(int(t['id']))

            # if `frame` is not present, means the thread is running.
            # in this case, `state` is present.
            if "frame" in t:
                file_loc = f" at {t['frame']['file']}:{t['frame']['line']}" if 'file' in t['frame'] else ''
                out_entries.append(
                    (tid, f"\t{tid}\t{t['target-id']}\t{t['frame']['func']}{file_loc}")
                )
            else:
                out_entries.append(
                    (tid, f"\t{tid}\t{t['target-id']}\t{t['state']}")
                )

        out_entries = sorted(out_entries, key=lambda x: x[0])
        out_str += "\n".join([ e[1] for e in out_entries ])
        # out_str = utils.wrap_grouped_message(out_str)
        return out_str

''' Handling `thread-group-*` related async record
'''
class ThreadGroupNotifTransformer(TransformerBase):
    def __init__(self, gtgid: int) -> None:
        super().__init__()
        self.gtgid = gtgid

    def transform(self, responses: List[SessionResponse]) -> dict:
        assert(len(responses) == 1)
        response = responses[0]
        payload = response.payload.copy() 
        payload["id"] = f"i{self.gtgid}"
        return payload

    def format(self, responses: List[SessionResponse]) -> str:
        # Example Output
        # =thread-group-added,id="i1"
        # =thread-group-removed,id="id"
        # =thread-group-started,id="id"
        # =thread-group-exited,id="id"[,exit-code="code"]
        data = self.transform(responses)
        assert(len(responses) == 1)
        response = responses[0]
        out_str = MIFormatter.format("=", response.msg, data, response.token)
        # out_str = f"=thread-group-added,id=\"i{self.gtgid}\"\n"
        return out_str 

''' Handling `thread-created` async record
'''
class ThreadCreatedNotifTransformer(TransformerBase):
    def __init__(self, gtid: int, gtgid: int) -> None:
        super().__init__()
        self.gtid = gtid
        self.gtgid = gtgid

    def transform(self, responses: List[SessionResponse]) -> dict:
        pass

    def format(self, responses: List[SessionResponse]) -> str:
        # Example Output
        # =thread-created,id="1",group-id="i1"
        out_str = f"=thread-created,id=\"{self.gtid}\",group-id=\"i{self.gtgid}\"\n"
        return out_str 
class ThreadExitedNotifTransformer(TransformerBase):
    def __init__(self, gtid: int, gtgid: int) -> None:
        super().__init__()
        self.gtid = gtid
        self.gtgid = gtgid

    def transform(self, responses: List[SessionResponse]) -> dict:
        pass

    def format(self, responses: List[SessionResponse]) -> str:
        # Example Output
        # =thread-created,id="1",group-id="i1"
        out_str = f"=thread-exited,id=\"{self.gtid}\",group-id=\"i{self.gtgid}\"\n"
        return out_str 
''' Handling `running` async record
'''
class RunningAsyncRecordTransformer(TransformerBase):
    def __init__(self, all_running: bool = False) -> None:
        super().__init__()
        self.all_running = all_running

    def iter_over_threads(self, response: SessionResponse) -> str:
        out = "" 
        for gtid in StateManager.inst().get_gtids_by_sid(response.sid):
            out += f"*running,thread-id=\"{gtid}\"\n"
        return out

    def transform(self, responses: List[SessionResponse]) -> dict:
        pass

    def format(self, responses: List[SessionResponse]) -> str:
        response = responses[0]

        # Example Output
        # *running,thread-id="all"
        # *running,thread-id="2"
        out_str = None
        if self.all_running:
            out_str = self.iter_over_threads(response)
        else:
            tid = int(response.response["payload"]["thread-id"])
            gtid = StateManager.inst().get_gtid(response.sid, tid)
            out_str = f"*running,thread-id=\"{gtid}\"\n"
        return out_str 

''' Handling `stopped` async record
'''
class StopAsyncRecordTransformer(TransformerBase):
    def __init__(self) -> None:
        super().__init__()

    def transform(self, responses: List[SessionResponse]) -> dict:
        assert(len(responses) == 1)  
        response = responses[0]
        # Example Output
        # https://github.com/USC-NSL/distributed-debugger/issues/24#issuecomment-1938140846

        payload = response.payload.copy()
        payload["thread-id"] = StateManager.inst().get_gtid(response.sid, int(payload["thread-id"]))
        payload["session-id"] = response.sid
        stopped_threads = payload["stopped-threads"]
        new_stopped_threads = []
        if isinstance(stopped_threads, list):
            for t in stopped_threads:
                new_stopped_threads.append(StateManager.inst().get_gtid(response.sid, int(t)))
        else:
            if isinstance(stopped_threads, str) and stopped_threads == "all":
                for gtid in StateManager.inst().get_gtids_by_sid(response.sid):
                    new_stopped_threads.append(gtid)
            else:
                utils.eprint(f"Unknown stopped-threads format: {stopped_threads}")
                return

        payload["stopped-threads"] = new_stopped_threads
        return payload

    def format(self, responses: List[SessionResponse]) -> str:
        payload = self.transform(responses)
        response = responses[0]
        # Example Output
        # https://github.com/USC-NSL/distributed-debugger/issues/24#issuecomment-1938140846
        return MIFormatter.format("*", "stopped", payload, response.token)

''' Handling generic `stopped` async record (not thread-related)
'''
class GenericStopAsyncRecordTransformer(TransformerBase):
    def __init__(self) -> None:
        super().__init__()

    def transform(self, responses: List[SessionResponse]) -> dict:
        return responses[0].payload

    def format(self, responses: List[SessionResponse]) -> str:
        payload = self.transform(responses)
        response = responses[0]
        return MIFormatter.format("*", "stopped", payload, response.token)

''' Handling `-stack-list-frames`
'''
class StackListFramesTransformer(TransformerBase):
    def __init__(self) -> None:
        super().__init__()

    def transform(self, responses: List[SessionResponse]) -> dict:
        assert(len(responses) == 1)  
        response = responses[0]
        return response.payload

    def format(self, responses: List[SessionResponse]) -> str:
        payload = self.transform(responses)
        return MIFormatter.format("^", "done", payload, None)
''' Handling `-thread-select`
'''
class ThreadSelectTransformer(TransformerBase):
    def __init__(self,gtid) -> None:
        super().__init__()
        self.gtid=str(gtid)

    def transform(self, responses: List[SessionResponse]) -> dict:
        assert(len(responses) == 1)  
        payload = responses[0].payload
        payload["new-thread-id"]=self.gtid
        return payload

    def format(self, responses: List[SessionResponse]) -> str:
        payload = self.transform(responses)
        response=responses[0]
        return MIFormatter.format("^", "done", payload, response.token)

''' Handling `bt`, `backtrace`, `where` commands
'''
class BacktraceReadableTransformer(TransformerBase):
    def __init__(self) -> None:
        super().__init__()

    def transform(self, responses: List[SessionResponse]) -> dict:
        pass

    def format(self, responses: List[SessionResponse]) -> str:
        if responses[0].msg == "error":
            return ErrorResponseTransformer().format(responses)
        payload = responses[0].payload
        stacks = payload["stack"]
        out_str = ""
        for i, stack in enumerate(stacks):
            level = stack["level"]
            func = stack["func"]
            addr = stack["addr"]
            filename = stack["file"] if "file" in stack else None
            line = stack["line"] if "line" in stack else None
            if filename and line:
                if i == 0:
                    out_str += f"#{level} {func} at {filename}:{line}\n" 
                else:
                    out_str += f"#{level} {addr} in {func} at {filename}:{line}\n"
            else:
                out_str += f"#{level} {addr} in {func}\n"
        return out_str

""" Handling `error` response
"""
class ErrorResponseTransformer(TransformerBase):
    def __init__(self) -> None:
        super().__init__()

    def transform(self, responses: List[SessionResponse]) -> dict:
        pass

    def format(self, responses: List[SessionResponse]) -> str:
        payload = responses[0].payload
        return MIFormatter.format("^", "error", payload, None) 

class ResponseTransformer:
    @staticmethod
    def transform(responses: List[SessionResponse], transformer: TransformerBase):
        if isinstance(responses, SessionResponse):
            responses = [ responses ]
        transformed_output=transformer.format(responses).replace("\n", "")
        if transformed_output is not None and len(transformed_output) > 0:
            print(f"\n[ TOOL MI OUTPUT ] \n{transformed_output}\n")
            logger.debug(f"\n[ TOOL MI OUTPUT ] \n{transformed_output}\n")

    @staticmethod
    def output(responses: Union[List[SessionResponse], SessionResponse], transformer: TransformerBase):
        """
        alias for transform function.
        """
        ResponseTransformer.transform(responses, transformer)
