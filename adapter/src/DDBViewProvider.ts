import * as vscode from 'vscode';
import axios from 'axios';
import { Breakpoint } from 'vscode-debugadapter';
import { logger } from './logger';

class SessionsCommandsProvider implements vscode.TreeDataProvider<SessionItem | SessionItemDetail | CommandItem | BreakPointItem> {
	private _onDidChangeTreeData: vscode.EventEmitter<SessionItem | CommandItem | undefined | null | void> = new vscode.EventEmitter<SessionItem | CommandItem | undefined | null | void>();
	readonly onDidChangeTreeData: vscode.Event<SessionItem | CommandItem | undefined | null | void> = this._onDidChangeTreeData.event;

	constructor(private apiBaseUrl: string, private breakpointSessionsMap: Map<string, string[]>) { }


	refresh(): void {
		this._onDidChangeTreeData.fire();
	}

	getTreeItem(element: SessionItem | CommandItem | BreakPointItem): vscode.TreeItem {
		return element;
	}

	async getChildren(element?: SessionItem | CommandItem | BreakPointItem | SessionItemDetail): Promise<(SessionItem | SessionItemDetail | CommandItem | BreakPointItem)[]> {
		if (!element) {
			// Root level: Sessions, Pending Commands, Finished Commands
			return [
				new SessionItem('Sessions', vscode.TreeItemCollapsibleState.Collapsed, false),
				new CommandItem('Pending Commands', vscode.TreeItemCollapsibleState.Collapsed, 'pending'),
				new CommandItem('Finished Commands', vscode.TreeItemCollapsibleState.Collapsed, 'finished'),
				new BreakPointItem('Breakpoint', [], vscode.TreeItemCollapsibleState.Collapsed),
			];
		} else if (element instanceof SessionItem) {
			if (element.label === 'Sessions') {
				// Fetch and return sessions
				return this.getSessions();
			}

			if (element.sessionDetails) {
				const sessionDetails = element.sessionDetails;
				const sessionDetailsItems: SessionItemDetail[] = [];
				for (const key in sessionDetails) {
					if (Object.prototype.hasOwnProperty.call(sessionDetails, key)) {
						const value = sessionDetails[key];
						const sessionDetailItem = new SessionItemDetail(
							key,
							vscode.TreeItemCollapsibleState.None,
							value
						);
						sessionDetailsItems.push(sessionDetailItem);
					}
				}
				return sessionDetailsItems
			}
		} else if (element instanceof CommandItem && element.label === 'Pending Commands') {
			// Fetch and return pending commands
			return this.getCommands('pending');
		} else if (element instanceof CommandItem && element.label === 'Finished Commands') {
			// Fetch and return finished commands
			return this.getCommands('finished');
		} else if (element instanceof BreakPointItem && element.label === 'Breakpoint') {
			return this.getBreakpointSessions();
		}
		return [];
	}
	private getBreakpointSessions(): BreakPointItem[] {
		const breakpointSessions: BreakPointItem[] = [];
		this.breakpointSessionsMap.forEach((sessions, breakpointId) => {
			breakpointSessions.push(new BreakPointItem(breakpointId, sessions, vscode.TreeItemCollapsibleState.None));
		});
		return breakpointSessions;
	}
	private async getSessions(): Promise<SessionItem[]> {
		try {
			const response = await axios.get(`${this.apiBaseUrl}/sessions`);
			// Return sessions with collapsible state to make them expandable
			return response.data.map((session: any) => {
				const sessionItem = new SessionItem(
					`[${session.alias}] ${session.tag}`,
					vscode.TreeItemCollapsibleState.Collapsed,
					true,
					session.status,
					String(session.sid),
					{
						"alias": String(session.alias),
						"sid": String(session.sid),
						"tag": session.tag,
					}
				);
				return sessionItem;
			});
		} catch (error) {
			vscode.window.showErrorMessage('Failed to fetch sessions');
			return [];
		}
	}

	private async getCommands(type: 'pending' | 'finished'): Promise<CommandItem[]> {
		try {
			const endpoint = type === 'pending' ? 'pcommands' : 'fcommands';
			const response = await axios.get(`${this.apiBaseUrl}/${endpoint}`);
			return response.data.map((cmd: any) =>
				new CommandItem(cmd.command, vscode.TreeItemCollapsibleState.None, type, cmd)
			).reverse();
		} catch (error) {
			vscode.window.showErrorMessage(`Failed to fetch ${type} commands`);
			return [];
		}
	}
}

class SessionItem extends vscode.TreeItem {
	private createButton(title: string, icon: string, command: string): string {
		const args = encodeURIComponent(JSON.stringify([this.sessionId]));
		return `<a href="command:${command}?${args}" title="${title}"><span style="color: var(--vscode-textLink-foreground);">$(${icon})</span></a>`;
	}

	constructor(
		public readonly label: string,
		public readonly collapsibleState: vscode.TreeItemCollapsibleState,
		public readonly showStatus: boolean,
		public readonly status?: string,
		public readonly sessionId?: string,
		public readonly sessionDetails?: any
	) {
		super(label, collapsibleState);
		this.sessionDetails = sessionDetails;

		// Add icons for expandable items
		// if (collapsibleState === vscode.TreeItemCollapsibleState.Collapsed || 
		// 	collapsibleState === vscode.TreeItemCollapsibleState.Expanded) {
		// 	this.iconPath = new vscode.ThemeIcon('debug-session');
		// }

		if (showStatus) {
			this.description = this.status;
			this.sessionId = sessionId;
			// Add a context value to enable right-click menu actions
			this.contextValue = 'sessionItem';
		}
	}
}

class SessionItemDetail extends vscode.TreeItem {
	constructor(
		public readonly label: string,
		public readonly collapsibleState: vscode.TreeItemCollapsibleState,
		public readonly description: string,
	) {
		super(label, collapsibleState);
		this.description = description;
	}
}

class CommandItem extends vscode.TreeItem {
	constructor(
		public readonly label: string,
		public readonly collapsibleState: vscode.TreeItemCollapsibleState,
		public readonly type: 'pending' | 'finished',
		public readonly commandData?: any
	) {
		super(label, collapsibleState);
		this.tooltip = this.label;
		if (commandData) {
			this.description = `${commandData.target_sessions.length}/${commandData.finished_sessions.length}`
			this.tooltip = `Token: ${commandData.token}\nCommand: ${commandData.command}\nTarget Sessions: ${commandData.target_sessions.join(', ')}\nFinished Sessions: ${commandData.finished_sessions.join(', ')}`;
		}
		// this.iconPath = new vscode.ThemeIcon(type === 'pending' ? 'loading~spin' : 'pass');
	}
}

class BreakPointItem extends vscode.TreeItem {
	constructor(
		public readonly name: string,
		public readonly sessions: string[],
		public readonly collapsibleState: vscode.TreeItemCollapsibleState,
	) {
		super(name, collapsibleState);

		this.label = name;

		// Display session IDs directly in the description
		this.description = sessions.join(', ');

		// Create a detailed tooltip with session information
		this.tooltip = new vscode.MarkdownString(`**${name}**\n\nSessions:\n${sessions.map(s => `- ${s}`).join('\n')}`);
	}
}

let periodicRefreshIntervalId: NodeJS.Timeout | undefined;
export function activate(context: vscode.ExtensionContext, breakpointSessionsMap: Map<string, string[]>) {
	const apiBaseUrl = process.env.DDB_API_URL || 'http://localhost:5000';
	const sessionsCommandsProvider = new SessionsCommandsProvider(apiBaseUrl, breakpointSessionsMap);
	// vscode.window.registerTreeDataProvider('sessionsCommandsExplorer', sessionsCommandsProvider);
	const treeView = vscode.window.createTreeView('sessionsCommandsExplorer', {
		treeDataProvider: sessionsCommandsProvider
	});

	context.subscriptions.push(treeView); // Add TreeView to subscriptions

	const visibilityListener = treeView.onDidChangeVisibility(e => {
		sessionsCommandsProvider.refresh();
	});
	sessionsCommandsProvider.refresh();
	context.subscriptions.push(visibilityListener); // Add listener disposable to subscriptions
	const refreshIntervalMs = 1000;
	const debugStartListener = vscode.debug.onDidStartDebugSession(debugSession => {
		periodicRefreshIntervalId = setInterval(() => {
			if (treeView.visible) {
				sessionsCommandsProvider.refresh();
			}
		}, refreshIntervalMs);
		sessionsCommandsProvider.refresh();
	});

	const debugStopListener = vscode.debug.onDidTerminateDebugSession(debugSession => {
		clearInterval(periodicRefreshIntervalId);
	});
	// Add the listener disposable to subscriptions for cleanup
	context.subscriptions.push(debugStartListener);
	context.subscriptions.push(debugStopListener);
	const refreshCommand = vscode.commands.registerCommand('sessionsCommandsExplorer.refresh', () => sessionsCommandsProvider.refresh());

	context.subscriptions.push(
		vscode.commands.registerCommand('sessionsCommandsExplorer.pauseSession', (item: SessionItem) => {
			const sessionId = item.sessionId;
			vscode.window.showInformationMessage(`Trying to pause session: ${sessionId}`);
			const debugSession = vscode.debug.activeDebugSession;
			debugSession.customRequest('pause', { sessionId: sessionId });
		})
	);

	context.subscriptions.push(
		vscode.commands.registerCommand('sessionsCommandsExplorer.continueSession', (item: SessionItem) => {
			const sessionId = item.sessionId;
			vscode.window.showInformationMessage(`Trying to continue session: ${sessionId}`);
			const debugSession = vscode.debug.activeDebugSession;
			debugSession.customRequest('continue', { sessionId: sessionId });

		})
	);
	context.subscriptions.push(refreshCommand);



}

export function deactivate() { }