import * as DebugAdapter from 'vscode-debugadapter';
import { DebugSession, InitializedEvent, TerminatedEvent, StoppedEvent, ThreadEvent, OutputEvent, ContinuedEvent, Thread, StackFrame, Scope, Source, Handles } from 'vscode-debugadapter';
import { DebugProtocol } from 'vscode-debugprotocol';
import { Breakpoint, IBackend, Variable, VariableObject, ValuesFormattingMode, MIError, SingleBreakpoint } from './backend/backend';
import { MINode } from './backend/mi_parse';
import { expandValue, isExpandable } from './backend/gdb_expansion';
import { MI2 } from './backend/mi2/mi2';
import { execSync } from 'child_process';
import * as systemPath from "path";
import * as net from "net";
import * as os from "os";
import * as fs from "fs";
import { SourceFileMap } from "./source_file_map";
import { setFlagsFromString } from 'v8';
import { Debugger } from 'inspector';
import { send } from 'process';
import { cloneDeep } from 'lodash';
const trace = process.env.TRACE?.toLowerCase() === 'true';
class ExtendedVariable {
	constructor(public name: string, public options: { "arg": any }) {
	}
}

class VariableScope {
	constructor(public readonly name: string, public readonly threadId: number, public readonly level: number, public readonly session: number) {
	}

	public static variableName(handle: number, name: string): string {
		return `var_${handle}_${name}`;
	}
}

export enum RunCommand { CONTINUE, RUN, NONE }

type M_Thread = {
	id: number;
	name: string;
	groupId: string;
}
let fakeThreadId = -1;
export class MI2DebugSession extends DebugSession {
	protected variableHandles = new Handles<VariableScope | string | VariableObject | ExtendedVariable>();
	protected variableHandlesReverse: { [id: string]: number } = {};
	protected scopeHandlesReverse: { [key: string]: number } = {};
	protected threadIdToSessionId: Map<number, number> = new Map();
	protected useVarObjects: boolean;
	protected quit: boolean;
	protected attached: boolean;
	protected initialRunCommand: RunCommand;
	protected stopAtEntry: boolean | string;
	protected isSSH: boolean;
	protected sourceFileMap: SourceFileMap;
	protected started: boolean;
	protected crashed: boolean;
	protected miDebugger: MI2;
	protected commandServer: net.Server;
	protected serverPath: string;
	protected m_threads: Map<number, M_Thread> = new Map();
	protected goToTargets: Map<number, DebugProtocol.GotoTarget & { path: string }> = new Map();

	public constructor(debuggerLinesStartAt1: boolean, isServer: boolean = false) {
		super(debuggerLinesStartAt1, isServer);
	}

	protected initDebugger() {
		this.miDebugger.on("launcherror", this.launchError.bind(this));
		this.miDebugger.on("quit", this.quitEvent.bind(this));
		this.miDebugger.on("exited-normally", this.quitEvent.bind(this));
		this.miDebugger.on("stopped", this.stopEvent.bind(this));
		this.miDebugger.on("msg", this.handleMsg.bind(this));
		this.miDebugger.on("breakpoint", this.handleBreakpoint.bind(this));
		this.miDebugger.on("watchpoint", this.handleBreak.bind(this));	// consider to parse old/new, too (otherwise it is in the console only)
		this.miDebugger.on("step-end", this.handleBreak.bind(this));
		//this.miDebugger.on("step-out-end", this.handleBreak.bind(this));  // was combined into step-end
		this.miDebugger.on("step-other", this.handleBreak.bind(this));
		this.miDebugger.on("signal-stop", this.handlePause.bind(this));
		this.miDebugger.on("thread-created", this.threadCreatedEvent.bind(this));
		this.miDebugger.on("thread-exited", this.threadExitedEvent.bind(this));
		this.miDebugger.on("running", this.runningEvent.bind(this))
		this.miDebugger.once("debug-ready", (() => this.sendEvent(new InitializedEvent())));
		try {
			this.commandServer = net.createServer(c => {
				c.on("data", data => {
					const rawCmd = data.toString();
					const spaceIndex = rawCmd.indexOf(" ");
					let func = rawCmd;
					let args = [];
					if (spaceIndex != -1) {
						func = rawCmd.substring(0, spaceIndex);
						args = JSON.parse(rawCmd.substring(spaceIndex + 1));
					}
					Promise.resolve((this.miDebugger as any)[func].apply(this.miDebugger, args)).then(data => {
						c.write(data.toString());
					});
				});
			});
			this.commandServer.on("error", err => {
				if (process.platform != "win32")
					this.handleMsg("stderr", "Code-Debug WARNING: Utility Command Server: Error in command socket " + err.toString() + "\nCode-Debug WARNING: The examine memory location command won't work");
			});
			if (!fs.existsSync(systemPath.join(os.tmpdir(), "code-debug-sockets")))
				fs.mkdirSync(systemPath.join(os.tmpdir(), "code-debug-sockets"));
			this.commandServer.listen(this.serverPath = systemPath.join(os.tmpdir(), "code-debug-sockets", ("Debug-Instance-" + Math.floor(Math.random() * 36 * 36 * 36 * 36).toString(36)).toLowerCase()));
		} catch (e) {
			if (process.platform != "win32")
				this.handleMsg("stderr", "Code-Debug WARNING: Utility Command Server: Failed to start " + e.toString() + "\nCode-Debug WARNING: The examine memory location command won't work");
		}
	}

	// verifies that the specified command can be executed
	protected checkCommand(debuggerName: string): boolean {
		try {
			const command = process.platform === 'win32' ? 'where' : 'command -v';
			execSync(`${command} ${debuggerName}`, { stdio: 'ignore' });
			return true;
		} catch (error) {
			return false;
		}
	}

	protected setValuesFormattingMode(mode: ValuesFormattingMode) {
		switch (mode) {
			case "disabled":
				this.useVarObjects = true;
				this.miDebugger.prettyPrint = false;
				break;
			case "prettyPrinters":
				this.useVarObjects = true;
				this.miDebugger.prettyPrint = true;
				break;
			case "parseText":
			default:
				this.useVarObjects = false;
				this.miDebugger.prettyPrint = false;
		}
	}

	protected handleMsg(type: string, msg: string) {
		if (type == "target")
			type = "stdout";
		if (type == "log")
			type = "stderr";
		this.sendEvent(new OutputEvent(msg, type));
	}

	protected handleBreakpoint(info: MINode) {

		new Promise((resolve, reject) => {
			this.miDebugger.sendCommand(`exec-interrupt`).then((info) => {
				resolve(info.resultRecords.resultClass == "done");
			}, reject);
		});
		const bp_thread_id = parseInt(info.record("thread-id"))
		const session_id = parseInt(info.record("session-id"))
		const event = new StoppedEvent("breakpoint", bp_thread_id);
		this.sendEvent(event);
		const stopped_threads: [] = info.record("stopped-threads")
		if (stopped_threads.length > 1) {
			for (const thread_id of stopped_threads) {
				if (parseInt(thread_id) == bp_thread_id) continue;
				// this.miDebugger.log("stderr", `sending stop event${parseInt(thread_id)}`)
				const event = new StoppedEvent("", parseInt(thread_id));
				//@ts-ignore
				event.body.preserveFocusHint = true
				this.sendEvent(event);
			}
		} else {
			const event = new StoppedEvent("", undefined);
			(event as DebugProtocol.StoppedEvent).body.allThreadsStopped = true;
			this.sendEvent(event);
		}
		this.sendEvent(new DebugAdapter.Event("breakpointCustom", {
			session_id: session_id,
			file: info.record("frame.file"),
			line: info.record("frame.line"),
		}))
	}

	protected handleBreak(info?: MINode) {
		if (trace)
			this.miDebugger.log("stderr", `handleBreak${JSON.stringify(info)}`)
		const stopped_threads: [] = info.record("stopped-threads")
		const step_thread_id = info ? parseInt(info.record("thread-id")) : 1
		this.sendEvent(new StoppedEvent("step", step_thread_id));
		if (stopped_threads.length > 1) {
			for (const thread_id of stopped_threads) {
				if (parseInt(thread_id) != step_thread_id) {
					// this.miDebugger.log("stderr", `sending stop event${parseInt(thread_id)}`)
					const event = new StoppedEvent("", parseInt(thread_id));
					//@ts-ignore
					event.body.preserveFocusHint = true
					this.sendEvent(event);
				}
			}
		} else {
			const event = new StoppedEvent("", undefined);
			(event as DebugProtocol.StoppedEvent).body.allThreadsStopped = true;
			this.sendEvent(event);
		}
		// const event = new StoppedEvent("step", info ? parseInt(info.record("thread-id")) : 1);
		// (event as DebugProtocol.StoppedEvent).body.allThreadsStopped = info ? info.record("stopped-threads") == "all" : true;
		// this.sendEvent(event);
	}

	protected handlePause(info: MINode) {
		if (trace)
			this.miDebugger.log("stderr", `handlePause${JSON.stringify(info)}`)
		const stopped_threads: [] = info.record("stopped-threads")
		if (stopped_threads.length > 1) {
			for (const thread_id of stopped_threads) {
				// this.miDebugger.log("stderr", `sending stop event${parseInt(thread_id)}`)
				const event = new StoppedEvent("", parseInt(thread_id));
				//@ts-ignore
				event.body.preserveFocusHint = true
				this.sendEvent(event);
				//@ts-ignore
			}
		} else {
			const event = new StoppedEvent("", undefined);
			(event as DebugProtocol.StoppedEvent).body.allThreadsStopped = true;
			this.sendEvent(event);
		}
	}

	protected stopEvent(info: MINode) {
		if (trace)
			this.miDebugger.log("stderr", `stopEvent${JSON.stringify(info)}`)
		if (!this.started)
			this.crashed = true;
		if (!this.quit) {
			const event = new StoppedEvent("exception", parseInt(info.record("thread-id")));
			(event as DebugProtocol.StoppedEvent).body.allThreadsStopped = info.record("stopped-threads") == "all";
			this.sendEvent(event);
		}
	}
	protected runningEvent(info: MINode["outOfBandRecord"][number]) {
		if (!this.quit) {
			const event = new ContinuedEvent(parseInt(MINode.valueOf(info.output, "thread-id")), false);
			this.sendEvent(event);
		}
	}

	protected threadCreatedEvent(info: MINode) {
		if (trace)
			this.miDebugger.log("stderr", `threadCreatedEvent${JSON.stringify(info)}`)
		var threadId = parseInt(info.record("id"));
		this.m_threads.set(
			threadId,
			{
				id: threadId,
				name: `[${info.record("session-alias")}]: Thread ${info.record("id")}, sid = ${info.record("session-id")}`,
				groupId: info.record("group-id"),
			}
		);
		this.threadIdToSessionId.set(
			threadId,
			parseInt(info.record("session-id"))
		);
		this.sendEvent(new ThreadEvent("started", info.record("id")));
	}

	protected threadExitedEvent(info: MINode) {
		if (trace)
			this.miDebugger.log("stderr", `threadExitedEvent${JSON.stringify(info)}`)
		var threadId = parseInt(info.record("id"));
		this.m_threads.delete(threadId);
		this.threadIdToSessionId.delete(threadId);
		this.sendEvent(new ThreadEvent("exited", info.record("id")));
	}

	protected quitEvent() {
		this.quit = true;
		this.sendEvent(new TerminatedEvent());

		if (this.serverPath)
			fs.unlink(this.serverPath, (err) => {
				// eslint-disable-next-line no-console
				console.error("Failed to unlink debug server");
			});
	}

	protected launchError(err: any) {
		this.handleMsg("stderr", "Could not start debugger process, does the program exist in filesystem?\n");
		this.handleMsg("stderr", err.toString() + "\n");
		this.quitEvent();
	}

	protected override disconnectRequest(response: DebugProtocol.DisconnectResponse, args: DebugProtocol.DisconnectArguments): void {
		if (trace)
			this.miDebugger.log("stderr", `disconnectRequest${JSON.stringify(args)}`)
		// if (this.attached)
		// 	this.miDebugger.detach();
		// else
		Promise.resolve()
			.then(() => this.miDebugger.stop())
			.then(() => {
				if (this.commandServer) {
					this.commandServer.close();
					this.commandServer = undefined;
				}
			})
			.then(() => {
				this.sendResponse(response);
			})
			.catch((error) => {
				console.error('Error during disconnect:', error);
				this.sendErrorResponse(response, {
					id: 1,
					format: "Error during disconnect: {error}",
					variables: { error: error.toString() },
					showUser: true
				});
			});
	}

	protected override async setVariableRequest(response: DebugProtocol.SetVariableResponse, args: DebugProtocol.SetVariableArguments): Promise<void> {
		if (trace)
			this.miDebugger.log("stderr", `setVariableRequest${JSON.stringify(args)}`)
		try {
			const varId = this.variableHandlesReverse[args.name];
			const varObj = this.variableHandles.get(varId) as any;
			if (this.useVarObjects) {
				let name = args.name;
				const parent = this.variableHandles.get(args.variablesReference);
				if (parent instanceof VariableScope) {
					name = VariableScope.variableName(args.variablesReference, name);
				} else if (parent instanceof VariableObject) {
					name = `${parent.name}.${name}`;
				}

				const res = await this.miDebugger.varAssign(name, args.value);
				response.body = {
					value: res.result("value")
				};
			} else {
				await this.miDebugger.changeVariable(args.name, args.value);
				response.body = {
					value: args.value
				};
			}
			this.sendResponse(response);
		} catch (err) {
			this.sendErrorResponse(response, 11, `Could not continue: ${err}`);
		}
	}

	protected override setFunctionBreakPointsRequest(response: DebugProtocol.SetFunctionBreakpointsResponse, args: DebugProtocol.SetFunctionBreakpointsArguments): void {
		if (trace) {
			this.miDebugger.log("stderr", `setFunctionBreakPointsRequest${JSON.stringify(args)}`)
			this.miDebugger.log("stderr", `setFunctionBreakPointsRequestResponse${JSON.stringify(response)}`)
		}
		const all = [];
		args.breakpoints.forEach(brk => {
			all.push(this.miDebugger.addBreakPoint({ raw: brk.name, condition: brk.condition, countCondition: brk.hitCondition }));
		});
		Promise.all(all).then(brkpoints => {
			const finalBrks: DebugProtocol.Breakpoint[] = [];
			brkpoints.forEach(brkp => {
				if (brkp[0])
					finalBrks.push({ line: brkp[1].line, verified: true });
			});
			response.body = {
				breakpoints: finalBrks
			};
			this.sendResponse(response);
		}, msg => {
			this.sendErrorResponse(response, 10, msg.toString());
		});
	}

	protected override async customRequest(command: string, response: DebugProtocol.Response, args: any, request?: DebugProtocol.Request): Promise<void> {
		// return
		if (command.includes("setSessionBreakpoints")) {
			const bkptArgs = args.arguments as DebugProtocol.SetBreakpointsArguments;
			let sessionresponse = response as DebugProtocol.SetBreakpointsResponse;
			let path = bkptArgs.source.path;
			let transactionId = bkptArgs.transactionId;
			if (bkptArgs.sourceModified) {
				await this.miDebugger.clearBreakPoints(path);
			}
			const removePromises = [];
			for (const [pathLineId, bkpt] of this.miDebugger.breakpoints) {
				if (pathLineId.startsWith(path)) {
					const found = bkptArgs.breakpoints.find(brk => brk.line == this.miDebugger.getLineFromBreakpointId(pathLineId))
					if (!found) {
						for (const [_, singleBkpt] of bkpt) {
							removePromises.push(this.miDebugger.removeSingleBreakPoint(singleBkpt));
						}
					}
				}
			}
			await Promise.all(removePromises);
			const breakpointsResponse: DebugProtocol.Breakpoint[] = []
			for (const bkpt of bkptArgs.breakpoints) {
				const bkptPathLineId = this.miDebugger.generateBreakpointId(path, bkpt.line)
				const existedBreakpoints = this.miDebugger.breakpoints.get(bkptPathLineId)
				const requestedBreakpoints: SingleBreakpoint[] = bkpt.sessionIds.map(sessionId => {
					return {
						file: path,
						line: bkpt.line,
						condition: bkpt.condition,
						hitCondition: bkpt.hitCondition,
						logMessage: bkpt.logMessage,
						sessionId: sessionId,
					}
				})
				//current may not needed
				existedBreakpoints?.forEach(existedSingleBreakpoint => {
					if (!requestedBreakpoints.some(requestedBreakpoint => requestedBreakpoint.sessionId == existedSingleBreakpoint.sessionId)) {
						this.miDebugger.removeSingleBreakPoint(existedSingleBreakpoint)
					}
				})
				const addBkptPromises = []
				//reuse existing ones
				for (const singleBreakpoint of requestedBreakpoints) {
					addBkptPromises.push(async () => {
						if (existedBreakpoints?.has(singleBreakpoint.sessionId)) {
							const needUpdate = singleBreakpoint.condition != bkpt.condition || singleBreakpoint.hitCondition != bkpt.hitCondition || singleBreakpoint.logMessage != bkpt.logMessage
							if (needUpdate) {
								await this.miDebugger.removeSingleBreakPoint(singleBreakpoint);
							} else {
								// breakpointsResponse.push({
								// 	line: singleBreakpoint.line,
								// 	verified: singleBreakpoint.verified,
								// });
								return;
							}
						}
						await this.miDebugger.addSingleBreakPoint({ file: path, line: bkpt.line, condition: bkpt.condition, countCondition: bkpt.hitCondition, logMessage: bkpt.logMessage, sessionId: singleBreakpoint.sessionId })
					})
				}
				await Promise.all(addBkptPromises.map(p => p()))
			}
			//we return all the successful breakpoints
			//only return the breakpoints are in the same file
			const allResponse = []
			for (const [bkptId, sessionIds] of this.miDebugger.breakpoints) {
				const breakpoint = {
					line: this.miDebugger.getLineFromBreakpointId(bkptId),
					verified: sessionIds.size > 0,
					sessionIds: Array.from(sessionIds.keys()),
					source: {
						name: this.miDebugger.getFileFromBreakpointId(bkptId).split("/").pop(),
						path: this.miDebugger.getFileFromBreakpointId(bkptId),
					},
				};
				allResponse.push(breakpoint);

				if (bkptId.startsWith(path)) {
					breakpointsResponse.push(breakpoint);
				}
			}
			const bkptResponse = cloneDeep(sessionresponse)
			sessionresponse.body = {
				breakpoints: breakpointsResponse
			}
			this.bkptRequests[args.seq][0](sessionresponse);
			bkptResponse.body = {
				breakpoints: allResponse
			}
			bkptResponse.transactionId = transactionId
			this.sendResponse(bkptResponse);
		}
		// continue
		if (command == "continue") {
			const session_id = args.arguments.session_id
			let sessionId = -1;
			new Promise((resolve, reject) => {
				if (trace)
					this.miDebugger.log("stderr", `custom continueRequest session_id: ${session_id}`)
				this.miDebugger.sendCommand(`record-time-and-continue --session ${session_id}`).then((info) => {
					resolve(info.resultRecords.resultClass == "done");
				}, reject);
				// this.miDebugger.sendCommand(`exec-continue --session ${session_id}`).then((info) => {
				// 	resolve(info.resultRecords.resultClass == "done");
				// }, reject);
			}).then(done => {
				this.sendResponse(response);
			}, msg => {
				this.sendErrorResponse(response, 2, `Could not continue: ${msg}`);
			});
		}
	}
	private bkptRequests: Record<number, {}> = []
	private bkptmap = new Map<string, DebugProtocol.SourceBreakpoint[]>()
	protected override setBreakPointsRequest(response: DebugProtocol.SetBreakpointsResponse, args: DebugProtocol.SetBreakpointsArguments): void {
		if (trace) {
			this.miDebugger.log("stderr", `setBreakPointsRequest${JSON.stringify(args, null, 2)}`)
			this.miDebugger.log("stderr", `setBreakPointsRequestResponse${JSON.stringify(response, null, 2)}`)
		}
		const waitForAysyncSession = new Promise<DebugProtocol.SetBreakpointsResponse>((resolve, reject) => {
			this.bkptRequests[response.request_seq] = [resolve, reject];

		})
		waitForAysyncSession.then((cresponse) => {
			response.body = cresponse.body;
			if (trace)
				this.miDebugger.log("stderr", `setBreakPointsRequest send response ${JSON.stringify(response, null, 2)}`)
			this.sendResponse(response);
		}, (error) => {
			this.sendErrorResponse(response, 9, error.toString());
		});
	}
	private lastThreadsRequestTime: number = 0;
	private cachedThreadsResponse: DebugProtocol.ThreadsResponse | null = null;
	protected override threadsRequest(response: DebugProtocol.ThreadsResponse): void {
		// const now = Date.now();
		// if (now - this.lastThreadsRequestTime < 1000) {
		//     // Less than a second since the last request
		//     if (this.cachedThreadsResponse) {
		//         // Return the cached response immediately
		//         this.sendResponse(this.cachedThreadsResponse);
		//     } else {
		//         // If no cached response, send an empty response
		//         response.body = { threads: [] };
		//         this.sendResponse(response);
		//     }
		//     return;
		// }
		// this.lastThreadsRequestTime = now;
		if (trace) {
			this.miDebugger.log("stderr", `threadsRequest`)
		}
		if (!this.miDebugger) {
			this.sendResponse(response);
			return;
		}
		let groupedThreads = new Map<string, M_Thread[]>();
		for (const [key, value] of this.m_threads) {
			if (!groupedThreads.has(value.groupId)) {
				groupedThreads.set(value.groupId, []);
			}
			groupedThreads.get(value.groupId)?.push(value);
		}
		const threads: DebugProtocol.Thread[] = [];
		for (const [groupId, groupThreads] of groupedThreads) {
			threads.push(
				{
					id: fakeThreadId--,
					name: `📦 ${groupId}`,
				}
			)
			for (const thread of groupThreads) {
				threads.push(
					{
						id: thread.id,
						name: thread.name,
					}
				)
			}
		}
		response.body = {
			threads: threads
		};
		this.sendResponse(response);
		// this.miDebugger.getThreads().then(threads => {

		// 	threads.sort((a, b) => a.id - b.id);
		// 	for (const thread of threads) {
		// 		const threadName = thread.name || thread.targetId || "<unnamed>";
		// 		response.body.threads.push(new Thread(thread.id, thread.id + ":" + threadName));
		// 	}
		// 	this.miDebugger.log("stderr", `threadsRequest send response ${response.seq}`)

		// 	this.cachedThreadsResponse = response;
		// }).catch((error: MIError) => {
		// 	if (error.message === 'Selected thread is running.') {
		// 		this.sendResponse(response);
		// 		return;
		// 	}
		// 	this.sendErrorResponse(response, 17, `Could not get threads: ${error}`);
		// });
	}

	// Supports 65535 threads.
	protected threadAndLevelToFrameId(threadId: number, level: number, sessionId: number) {

		return level << 16 | threadId | sessionId << 24;
	}
	protected frameIdToThreadAndLevelAndSessionId(frameId: number): [number, number, number] {
		const threadId = frameId & 0xffff;
		const level = (frameId >> 16) & 0xff;
		const sessionId = frameId >>> 24;
		return [threadId, level, sessionId];
	}

	protected override stackTraceRequest(response: DebugProtocol.StackTraceResponse, args: DebugProtocol.StackTraceArguments): void {
		if (trace)
			this.miDebugger.log("stderr", `stackTraceRequest${JSON.stringify(args)}`)
		this.miDebugger.getStack(args.startFrame, args.levels, args.threadId).then(stack => {
			const ret: StackFrame[] = [];
			stack.forEach(element => {
				let source = undefined;
				let path = element.file;
				if (path) {
					// if (this.isSSH) {
					// 	// convert ssh path to local path
					// 	path = this.sourceFileMap.toLocalPath(path);
					// } else if (process.platform === "win32") {
					// 	if (path.startsWith("\\cygdrive\\") || path.startsWith("/cygdrive/")) {
					// 		path = path[10] + ":" + path.substring(11); // replaces /cygdrive/c/foo/bar.txt with c:/foo/bar.txt
					// 	}
					// }
					source = new Source(element.fileName, path);
				}

				ret.push(new StackFrame(
					this.threadAndLevelToFrameId(element.thread, element.level, element.session),
					element.function + "@" + element.address,
					source,
					element.line,
					0));
			});

			// Apply the startFrame and levels filter to the stack frames
			const filteredStack = ret.slice(args.startFrame, (args.levels !== 0) ? args.startFrame + args.levels : undefined);

			response.body = {
				stackFrames: filteredStack,
				totalFrames: stack.length
			};
			this.sendResponse(response);
		}, err => {
			this.sendErrorResponse(response, 12, `Failed to get Stack Trace: ${err.toString()}`);
		});
	}

	protected override configurationDoneRequest(response: DebugProtocol.ConfigurationDoneResponse, args: DebugProtocol.ConfigurationDoneArguments): void {
		const promises: Thenable<any>[] = [];
		let entryPoint: string | undefined = undefined;
		let runToStart: boolean = false;
		// Setup temporary breakpoint for the entry point if needed.
		switch (this.initialRunCommand) {
			case RunCommand.CONTINUE:
			case RunCommand.NONE:
				if (typeof this.stopAtEntry == 'boolean' && this.stopAtEntry)
					entryPoint = "main"; // sensible default
				else if (typeof this.stopAtEntry == 'string')
					entryPoint = this.stopAtEntry;
				break;
			case RunCommand.RUN:
				if (typeof this.stopAtEntry == 'boolean' && this.stopAtEntry) {
					if (this.miDebugger.features.includes("exec-run-start-option"))
						runToStart = true;
					else
						entryPoint = "main"; // sensible fallback
				} else if (typeof this.stopAtEntry == 'string')
					entryPoint = this.stopAtEntry;
				break;
			default:
				throw new Error('Unhandled run command: ' + RunCommand[this.initialRunCommand]);
		}
		if (entryPoint)
			promises.push(this.miDebugger.setEntryBreakPoint(entryPoint));
		switch (this.initialRunCommand) {
			case RunCommand.CONTINUE:
				promises.push(this.miDebugger.continue().then(() => {
					// Some debuggers will provide an out-of-band status that they are stopped
					// when attaching (e.g., gdb), so the client assumes we are stopped and gets
					// confused if we start running again on our own.
					//
					// If we don't send this event, the client may start requesting data (such as
					// stack frames, local variables, etc.) since they believe the target is
					// stopped.  Furthermore, the client may not be indicating the proper status
					// to the user (may indicate stopped when the target is actually running).
					this.sendEvent(new ContinuedEvent(1, true));
				}));
				break;
			case RunCommand.RUN:
				promises.push(this.miDebugger.start(runToStart).then(() => {
					this.started = true;
					if (this.crashed)
						this.handlePause(undefined);
				}));
				break;
			case RunCommand.NONE: {
				// Not all debuggers seem to provide an out-of-band status that they are stopped
				// when attaching (e.g., lldb), so the client assumes we are running and gets
				// confused when we don't actually run or continue.  Therefore, we'll force a
				// stopped event to be sent to the client (just in case) to synchronize the state.
				const event: DebugProtocol.StoppedEvent = new StoppedEvent("pause", 1);
				event.body.description = "paused on attach";
				event.body.allThreadsStopped = true;
				this.sendEvent(event);
				break;
			}
			default:
				throw new Error('Unhandled run command: ' + RunCommand[this.initialRunCommand]);
		}
		Promise.all(promises).then(() => {
			this.sendResponse(response);
		}).catch(err => {
			this.sendErrorResponse(response, 18, `Could not run/continue: ${err.toString()}`);
		});
	}

	protected override scopesRequest(response: DebugProtocol.ScopesResponse, args: DebugProtocol.ScopesArguments): void {
		const scopes = new Array<Scope>();
		const [threadId, level, session_id] = this.frameIdToThreadAndLevelAndSessionId(args.frameId);

		const createScope = (scopeName: string, expensive: boolean): Scope => {
			const key: string = scopeName + ":" + threadId + ":" + level + ":" + session_id;
			let handle: number;

			if (this.scopeHandlesReverse.hasOwnProperty(key)) {
				handle = this.scopeHandlesReverse[key];
			} else {
				handle = this.variableHandles.create(new VariableScope(scopeName, threadId, level, session_id));
				this.scopeHandlesReverse[key] = handle;
			}

			return new Scope(scopeName, handle, expensive);
		};

		scopes.push(createScope("Locals", false));
		scopes.push(createScope("Registers", false));

		response.body = {
			scopes: scopes
		};
		this.sendResponse(response);
	}

	protected override async variablesRequest(response: DebugProtocol.VariablesResponse, args: DebugProtocol.VariablesArguments): Promise<void> {
		if (trace)
			this.miDebugger.log("stderr", `variablesRequest${JSON.stringify(args)}`)
		const variables: DebugProtocol.Variable[] = [];
		const id: VariableScope | string | VariableObject | ExtendedVariable = this.variableHandles.get(args.variablesReference);

		const createVariable = (arg: string | VariableObject, options?: any) => {
			if (options)
				return this.variableHandles.create(new ExtendedVariable(typeof arg === 'string' ? arg : arg.name, options));
			else
				return this.variableHandles.create(arg);
		};

		const findOrCreateVariable = (varObj: VariableObject): number => {
			let id: number;
			if (this.variableHandlesReverse.hasOwnProperty(varObj.name)) {
				id = this.variableHandlesReverse[varObj.name];
			} else {
				id = createVariable(varObj);
				this.variableHandlesReverse[varObj.name] = id;
			}
			return varObj.isCompound() ? id : 0;
		};

		if (id instanceof VariableScope) {
			try {
				if (id.name == "Registers") {
					const registers = await this.miDebugger.getRegisters();
					for (const reg of registers) {
						variables.push({
							name: reg.name,
							value: reg.valueStr,
							variablesReference: 0
						});
					}
				} else {
					const stack: Variable[] = await this.miDebugger.getStackVariables(id.threadId, id.level, id.session);
					const variablePromises = stack.map(async (variable) => {
						if (this.useVarObjects) {
							try {
								const varObjName = VariableScope.variableName(args.variablesReference, variable.name);
								let varObj: VariableObject;
								try {
									let hasVar = this.variableHandlesReverse.hasOwnProperty(varObjName);
									if (!hasVar) {
										throw new Error("Variable object not found");
									}
									const changes = await this.miDebugger.varUpdate(id.threadId, id.level, varObjName);
									const changelist = changes.result("changelist");
									changelist.forEach((change: any) => {
										const name = MINode.valueOf(change, "name");
										const vId = this.variableHandlesReverse[name];
										const v = this.variableHandles.get(vId) as any;
										v.applyChanges(change);
									});
									const varId = this.variableHandlesReverse[varObjName];
									varObj = this.variableHandles.get(varId) as any;
								} catch (err) {
									if ((err instanceof MIError && (err.message == "Variable object not found" || err.message.endsWith("does not exist")))
										|| (err instanceof Error && err.message == "Variable object not found")) {
										varObj = await this.miDebugger.varCreate(id.threadId, id.level, variable.name, varObjName);
										const varId = findOrCreateVariable(varObj);
										varObj.nameToDisplay = variable.name;
										varObj.exp = variable.name;
										varObj.id = varId;
									} else {
										throw err;
									}
								}
								return varObj.toProtocolVariable();
							} catch (err) {
								return {
									name: variable.name,
									value: `<${err}>`,
									variablesReference: 0
								};
							}
						} else {
							if (variable.valueStr !== undefined) {
								let expanded = expandValue(createVariable, `{${variable.name}=${variable.valueStr})`, "", variable.raw);
								if (expanded) {
									if (typeof expanded[0] == "string")
										expanded = [
											{
												name: "<value>",
												value: prettyStringArray(expanded),
												variablesReference: 0
											}
										];
									return expanded[0];
								}
							} else
								return {
									name: variable.name,
									type: variable.type,
									value: "<unknown>",
									variablesReference: createVariable(variable.name)
								};
						}
					});
					response.body = {
						variables: await Promise.all(variablePromises)
					};
				}
				this.sendResponse(response);
			} catch (err) {
				this.sendErrorResponse(response, 1, `Could not expand variable: ${err}`);
			}
		} else if (typeof id == "string") {
			// Variable members
			let variable;
			try {
				// TODO: this evaluates on an (effectively) unknown thread for multithreaded programs.
				variable = await this.miDebugger.evalExpression(JSON.stringify(id), 0, 0, 0);
				try {
					let expanded = expandValue(createVariable, variable.result("value"), id, variable);
					if (!expanded) {
						this.sendErrorResponse(response, 2, `Could not expand variable`);
					} else {
						if (typeof expanded[0] == "string")
							expanded = [
								{
									name: "<value>",
									value: prettyStringArray(expanded),
									variablesReference: 0
								}
							];
						response.body = {
							variables: expanded
						};
						this.sendResponse(response);
					}
				} catch (e) {
					this.sendErrorResponse(response, 2, `Could not expand variable: ${e}`);
				}
			} catch (err) {
				this.sendErrorResponse(response, 1, `Could not expand variable: ${err}`);
			}
		} else if (typeof id == "object") {
			if (id instanceof VariableObject) {
				// Variable members
				let children: VariableObject[];
				try {
					children = await this.miDebugger.varListChildren(id.threadId, id.name, id);
					const vars = children.map(child => {
						const varId = findOrCreateVariable(child);
						child.id = varId;
						return child.toProtocolVariable();
					});

					response.body = {
						variables: vars
					};
					this.sendResponse(response);
				} catch (err) {
					this.sendErrorResponse(response, 1, `Could not expand variable: ${err}`);
				}
			} else if (id instanceof ExtendedVariable) {
				const varReq = id;
				if (varReq.options.arg) {
					const strArr: DebugProtocol.Variable[] = [];
					let argsPart = true;
					let arrIndex = 0;
					const submit = () => {
						response.body = {
							variables: strArr
						};
						this.sendResponse(response);
					};
					const addOne = async () => {
						// TODO: this evaluates on an (effectively) unknown thread for multithreaded programs.
						const variable = await this.miDebugger.evalExpression(JSON.stringify(`${varReq.name}+${arrIndex})`), 0, 0, 0);
						try {
							const expanded = expandValue(createVariable, variable.result("value"), varReq.name, variable);
							if (!expanded) {
								this.sendErrorResponse(response, 15, `Could not expand variable`);
							} else {
								if (typeof expanded == "string") {
									if (expanded == "<nullptr>") {
										if (argsPart)
											argsPart = false;
										else
											return submit();
									} else if (expanded[0] != '"') {
										strArr.push({
											name: "[err]",
											value: expanded,
											variablesReference: 0
										});
										return submit();
									}
									strArr.push({
										name: `[${(arrIndex++)}]`,
										value: expanded,
										variablesReference: 0
									});
									addOne();
								} else {
									strArr.push({
										name: "[err]",
										value: expanded,
										variablesReference: 0
									});
									submit();
								}
							}
						} catch (e) {
							this.sendErrorResponse(response, 14, `Could not expand variable: ${e}`);
						}
					};
					addOne();
				} else
					this.sendErrorResponse(response, 13, `Unimplemented variable request options: ${JSON.stringify(varReq.options)}`);
			} else {
				response.body = {
					variables: id
				};
				this.sendResponse(response);
			}
		} else {
			response.body = {
				variables: variables
			};
			this.sendResponse(response);
		}
	}

	protected override pauseRequest(response: DebugProtocol.ContinueResponse, args: DebugProtocol.ContinueArguments): void {
		// async handling
		if (trace)
			this.miDebugger.log("stderr", `pauseRequest${JSON.stringify(args)}`)
		let command = "exec-interrupt"
		//@ts-ignore
		if (args.sessionId != undefined) {
			//@ts-ignore
			command += ` --session ${args.sessionId}`
		} else {
			command += ` --all`
		}
		this.miDebugger.sendCommand(command)
		this.sendResponse(response);
		// .then(
		// 	(info) => {
		// 		if (info.resultRecords.resultClass === "done") {
		// 			this.sendResponse(response);
		// 		} else {
		// 			this.sendErrorResponse(response, 3, `Could not pause: unexpected result class`);
		// 		}
		// 	},
		// 	(error) => {
		// 		this.sendErrorResponse(response, 3, `Could not pause: ${error.message}`);
		// 	}
		// );
	}

	protected override reverseContinueRequest(response: DebugProtocol.ReverseContinueResponse, args: DebugProtocol.ReverseContinueArguments): void {
		if (trace)
			this.miDebugger.log("stderr", `reverseContinueRequest${JSON.stringify(args)}`)
		this.miDebugger.continue(true).then(done => {
			this.sendResponse(response);
		}, msg => {
			this.sendErrorResponse(response, 2, `Could not continue: ${msg}`);
		});
	}

	protected override continueRequest(response: DebugProtocol.ContinueResponse, args: DebugProtocol.ContinueArguments): void {
		if (trace)
			this.miDebugger.log("stderr", `continueRequest ${JSON.stringify(args)}`)
		// let command = "exec-continue"
		let command = "record-time-and-continue"
		//@ts-ignore
		if (args.sessionId != undefined) {
			//@ts-ignore
			command += ` --session ${args.sessionId}`
		} else {
			command += ` --all`
		}

		new Promise((resolve, reject) => {
			this.miDebugger.sendCommand(command).then((info) => {
				resolve(info.resultRecords.resultClass == "done");
			}, reject);
		}).then(done => {
			this.sendResponse(response);
		}, msg => {
			this.sendErrorResponse(response, 2, `Could not continue: ${msg}`);
		});

	}

	protected override stepBackRequest(response: DebugProtocol.StepBackResponse, args: DebugProtocol.StepBackArguments): void {
		if (trace)
			this.miDebugger.log("stderr", `stepBackRequest${JSON.stringify(args)}`)
		this.miDebugger.step(args.threadId, true).then(done => {
			this.sendResponse(response);
		}, msg => {
			this.sendErrorResponse(response, 4, `Could not step back: ${msg} - Try running 'target record-full' before stepping back`);
		});
	}

	protected override stepInRequest(response: DebugProtocol.NextResponse, args: DebugProtocol.NextArguments): void {
		if (trace)
			this.miDebugger.log("stderr", `stepInRequest${JSON.stringify(args)}`)
		this.miDebugger.step(args.threadId).then(done => {
			this.sendResponse(response);
		}, msg => {
			this.sendErrorResponse(response, 4, `Could not step in: ${msg}`);
		});
	}

	protected override stepOutRequest(response: DebugProtocol.NextResponse, args: DebugProtocol.NextArguments): void {
		if (trace)
			this.miDebugger.log("stderr", `stepOutRequest${JSON.stringify(args)}`)
		this.miDebugger.stepOut(args.threadId).then(done => {
			this.sendResponse(response);
		}, msg => {
			this.sendErrorResponse(response, 5, `Could not step out: ${msg}`);
		});
	}

	protected override nextRequest(response: DebugProtocol.NextResponse, args: DebugProtocol.NextArguments): void {
		if (trace)
			this.miDebugger.log("stderr", `nextRequest${JSON.stringify(args)}`)
		this.miDebugger.next(args.threadId).then(done => {
			this.sendResponse(response);
		}, msg => {
			this.sendErrorResponse(response, 6, `Could not step over: ${msg}`);
		});
	}
	private findOrCreateVariable = (varObj: VariableObject): number => {
		let id: number;
		if (this.variableHandlesReverse.hasOwnProperty(varObj.name)) {
			id = this.variableHandlesReverse[varObj.name];
		} else {
			id = this.createVariable(varObj);
			this.variableHandlesReverse[varObj.name] = id;
		}
		return varObj.isCompound() ? id : 0;
	};
	private createVariable = (arg: string | VariableObject, options?: any) => {
		if (options)
			return this.variableHandles.create(new ExtendedVariable(typeof arg === 'string' ? arg : arg.name, options));
		else
			return this.variableHandles.create(arg);
	};
	protected override async evaluateRequest(response: DebugProtocol.EvaluateResponse, args: DebugProtocol.EvaluateArguments): Promise<void> {
		if (trace)
			this.miDebugger.log("stderr", `evaluateRequest${JSON.stringify(args)}`)

		const [threadId, level, session_id] = this.frameIdToThreadAndLevelAndSessionId(args.frameId);
		if (args.context == "watch" || args.context == "hover") {
			try {
				const varObjName = VariableScope.variableName(args.frameId, args.expression);
				let varObj: VariableObject;
				try {
					let hasVar = this.variableHandlesReverse.hasOwnProperty(varObjName);
					if (!hasVar) {
						throw new Error("Variable object not found");
					}
					const changes = await this.miDebugger.varUpdate(threadId, level, varObjName);
					const changelist = changes.result("changelist");
					changelist.forEach((change: any) => {
						const name = MINode.valueOf(change, "name");
						const vId = this.variableHandlesReverse[name];
						const v = this.variableHandles.get(vId) as any;
						v.applyChanges(change);
					});
					const varId = this.variableHandlesReverse[varObjName];
					varObj = this.variableHandles.get(varId) as any;
				} catch (err) {
					if ((err instanceof MIError && (err.message == "Variable object not found" || err.message.endsWith("does not exist")))
						|| (err instanceof Error && err.message == "Variable object not found")) {
						varObj = await this.miDebugger.varCreate(threadId, level, args.expression, varObjName);
						const varId = this.findOrCreateVariable(varObj);
						varObj.exp = args.expression;
						varObj.id = varId;
					} else {
						throw err;
					}
				}
				response.body = {
					result: varObj.value,
					type: varObj.type,
					variablesReference: varObj.id,
				};
			} catch (err) {
				response.body = {
					result: `${err}`,
					variablesReference: 0
				}
				this.sendResponse(response);
				return;
			}
			this.sendResponse(response);
		} else {
			this.miDebugger.sendUserInput(args.expression, threadId, level).then(output => {
				if (typeof output == "undefined")
					response.body = {
						result: "",
						variablesReference: 0
					};
				else
					response.body = {
						result: JSON.stringify(output),
						variablesReference: 0
					};
				this.sendResponse(response);
			}, msg => {
				this.sendErrorResponse(response, 8, msg.toString());
			});
		}
	}

	protected override gotoTargetsRequest(response: DebugProtocol.GotoTargetsResponse, args: DebugProtocol.GotoTargetsArguments): void {
		if (trace)
			this.miDebugger.log("stderr", `gotoTargetsRequest${JSON.stringify(args)}`)
		const path: string = this.isSSH ? this.sourceFileMap.toRemotePath(args.source.path) : args.source.path;
		const targetId = this.goToTargets.size + 1;  // Generate monotonically increasing id
		const target = {
			id: targetId,
			label: args.source.name,
			column: args.column,
			line: args.line,
			path: path
		};
		this.goToTargets.set(targetId, target);

		response.body = {
			targets: [target]
		};
		this.sendResponse(response);
	}

	protected override gotoRequest(response: DebugProtocol.GotoResponse, args: DebugProtocol.GotoArguments): void {
		if (trace)
			this.miDebugger.log("stderr", `gotoRequest${JSON.stringify(args)}`)
		const sid = this.threadIdToSessionId.get(args.threadId);
		const targetId = args.targetId;
		const target = this.goToTargets.get(targetId);

		this.miDebugger.goto(target.path, target.line, sid).then(done => {
			this.sendResponse(response);
		}, msg => {
			this.sendErrorResponse(response, 16, `Could not jump: ${msg}`);
		});
	}

	protected setSourceFileMap(configMap: { [index: string]: string }, fallbackGDB: string, fallbackIDE: string): void {
		if (configMap === undefined) {
			this.sourceFileMap = new SourceFileMap({ [fallbackGDB]: fallbackIDE });
		} else {
			this.sourceFileMap = new SourceFileMap(configMap, fallbackGDB);
		}
	}

}

function prettyStringArray(strings: any) {
	if (typeof strings == "object") {
		if (strings.length !== undefined)
			return strings.join(", ");
		else
			return JSON.stringify(strings);
	} else return strings;
}
