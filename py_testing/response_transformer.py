from data_struct import SessionResponse
from typing import Any, List
from state_manager import StateManager
import utils

class TransformerBase:
    def __init__(self) -> None:
        # self.responses = responses
        pass
        
    def transform(self, responses: List[SessionResponse]) -> dict:
        raise NotImplementedError

    def format(self, responses: List[SessionResponse]) -> str:
        return self.transform(responses) 

    # def transform_stdout(self, responses: List)

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
        out_str = "\n".join(data["data"])
        out_str = utils.wrap_grouped_message(out_str)
        return out_str

''' Handling `-list-threads` response
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
                    t["id"] = StateManager.inst().get_gtid(sid, tid)
                    all_threads_info.append(t)

        all_threads_info = sorted(all_threads_info, key=lambda x: x["id"])

        # TODO: handle current-thread-id
        out_dict = { 
            "threads": all_threads_info,
            "current-thread-id": StateManager.inst().get_current_gthread()
        }
        return out_dict
        # out_str = utils.wrap_grouped_message(str(out_dict))
        # return out_str

    def format(self, responses: List[SessionResponse]) -> str:
        data = self.transform(responses)
        out_str = utils.wrap_grouped_message(str(data))
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
        out_str = utils.wrap_grouped_message(str(data))
        return out_str

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
                    "exec": p["executable"]
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
        out_str = utils.wrap_grouped_message(out_str)
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
            file_loc = f" at {t['frame']['file']}:{t['frame']['line']}" if 'file' in t['frame'] else ''
            out_entries.append(
                (tid, f"\t{tid}\t{t['target-id']}\t{t['frame']['func']}{file_loc}")
            )
        out_entries = sorted(out_entries, key=lambda x: x[0])
        out_str += "\n".join([ e[1] for e in out_entries ])
        out_str = utils.wrap_grouped_message(out_str)
        return out_str

class ResponseTransformer:
    @staticmethod
    def transform(responses: List[SessionResponse], transformer: TransformerBase):
        print(transformer.format(responses))
