import json
import threading
from iddb.cmd_router import CmdRouter
from iddb.cmd_tracker import CmdTracker
from iddb.state_manager import StateManager
from flask import Flask, jsonify, request




class DebuggerStatus:
    _instance = None
    _lock = threading.Lock()

    def __new__(cls):
        if cls._instance is None:
            with cls._lock:
                if cls._instance is None:
                    cls._instance = super(DebuggerStatus, cls).__new__(cls)
        return cls._instance

    def __init__(self):
        if not hasattr(self, 'initialized'):
            self.initialized = True


class FlaskApp:
    def __init__(self, **kwargs):
        self.app = Flask("ddbapiserver")
        if "router" not in kwargs:
            raise ValueError("'router' is a required argument")

        self.router: CmdRouter = kwargs["router"]
        self.DDB_up_and_running = False
        self.setup_routes()

    def setup_routes(self):
        self.app.route('/sessions', methods=['GET'])(self.get_sessions)
        self.app.route(
            '/pcommands', methods=['GET'])(self.get_pending_commands)
        self.app.route(
            '/fcommands', methods=['GET'])(self.get_finished_commands)
        self.app.route('/status', methods=['GET'])(self.get_status)
    def get_status(self):
        if self.DDB_up_and_running:
            return jsonify({"status": "up"}),200
        else:
            return jsonify({"status": "down"}),500

    def get_sessions(self):
        results = []
        for sid, session in StateManager.inst().sessions.items():
            s = StateManager.inst().sessions[sid]
            session_meta = {"sid": s.sid, "tag": s.tag}
            if session.session_obj.gdb_controller.is_open():
                session_meta["status"] = "on"
            else:
                session_meta["status"] = "off"
            results.append(session_meta)
        return jsonify(results)

    def get_pending_commands(self):
        cmd_tracker = CmdTracker.inst()
        results = []
        for cmd_token, waiting_cmd in cmd_tracker.waiting_cmds.items():
            results.append({"token": cmd_token, "command": waiting_cmd.command, "target_sessions": list(
                waiting_cmd.target_sessions), "finished_sessions": list(waiting_cmd.finished_sessions)})
        return jsonify(results)

    def get_finished_commands(self):
        cmd_tracker = CmdTracker.inst()
        results = []
        for cmd_token, waiting_cmd in cmd_tracker.finished_cmds.items():
            results.append({"token": cmd_token, "command": waiting_cmd.command, "target_sessions": list(
                waiting_cmd.target_sessions), "finished_sessions": list(waiting_cmd.finished_sessions)})
        return jsonify(results)
    def run(self, host='0.0.0.0', port=5000, debug=True):
        self.app.run(host=host, port=port, debug=debug)
