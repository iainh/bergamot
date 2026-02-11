/*
 *
 * Copyright (C) 2012-2019 Andrey Prygunkov <hugbug@users.sourceforge.net>
 * Copyright (C) 2024 Denis <denis@nzbget.com>
 * Copyright (C) 2026 Bergamot contributors
 *
 * This program is free software; you can redistribute it and/or modify
 * it under the terms of the GNU General Public License as published by
 * the Free Software Foundation; either version 2 of the License, or
 * (at your option) any later version.
 *
 * This program is distributed in the hope that it will be useful,
 * but WITHOUT ANY WARRANTY; without even the implied warranty of
 * MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
 * GNU General Public License for more details.
 *
 * You should have received a copy of the GNU General Public License
 * along with this program.  If not, see <https://www.gnu.org/licenses/>.
 */

/*
 * In this module:
 *   1) Download tab;
 *   2) Functions for html generation for downloads, also used from other modules (edit and add dialogs);
 *   3) Popup menus in downloads list.
 */

/*** DOWNLOADS TAB ***********************************************************/

var Downloads = (new function($)
{
	'use strict';

	// Controls
	let $DownloadsTable;
	let $DownloadsTabBadge;
	let $DownloadsTabBadgeEmpty;
	let $DownloadQueueEmpty;
	let $DownloadsRecordsPerPage;
	let $DownloadsTable_Name;
	let $PriorityMenu;
	let $CategoryMenu;

	// State
	let notification = null;
	let updateTabInfo;
	let groups = [];
	let groupsInitialized = false;
	let nameColumnWidth = null;
	let cached = false;
	let lastDownloadRate;
	const listGroupsSubs = [];

	const statusData = Object.freeze({
		'QUEUED': Object.freeze({ Text: 'QUEUED', PostProcess: false }),
		'FETCHING': Object.freeze({ Text: 'FETCHING', PostProcess: false }),
		'DOWNLOADING': Object.freeze({ Text: 'DOWNLOADING', PostProcess: false }),
		'QS_QUEUED': Object.freeze({ Text: 'QS-QUEUED', PostProcess: true }),
		'QS_EXECUTING': Object.freeze({ Text: 'QUEUE-SCRIPT', PostProcess: true }),
		'PP_QUEUED': Object.freeze({ Text: 'PP-QUEUED', PostProcess: true }),
		'PAUSED': Object.freeze({ Text: 'PAUSED', PostProcess: false }),
		'LOADING_PARS': Object.freeze({ Text: 'CHECKING', PostProcess: true }),
		'VERIFYING_SOURCES': Object.freeze({ Text: 'CHECKING', PostProcess: true }),
		'REPAIRING': Object.freeze({ Text: 'REPAIRING', PostProcess: true }),
		'VERIFYING_REPAIRED': Object.freeze({ Text: 'VERIFYING', PostProcess: true }),
		'RENAMING': Object.freeze({ Text: 'RENAMING', PostProcess: true }),
		'MOVING': Object.freeze({ Text: 'MOVING', PostProcess: true }),
		'POST_UNPACK_RENAMING': Object.freeze({ Text: 'POST-UNPACK-RENAMING', PostProcess: true }),
		'UNPACKING': Object.freeze({ Text: 'UNPACKING', PostProcess: true }),
		'EXECUTING_SCRIPT': Object.freeze({ Text: 'PROCESSING', PostProcess: true }),
		'PP_FINISHED': Object.freeze({ Text: 'FINISHED', PostProcess: false })
	});
	this.statusData = statusData;
	this.groups = groups;

	this.init = function(options)
	{
		updateTabInfo = options.updateTabInfo;

		$DownloadsTable = $('#DownloadsTable');
		$DownloadsTabBadge = $('#DownloadsTabBadge');
		$DownloadsTabBadgeEmpty = $('#DownloadsTabBadgeEmpty');
		$DownloadQueueEmpty = $('#DownloadQueueEmpty');
		$DownloadsRecordsPerPage = $('#DownloadsRecordsPerPage');
		$DownloadsTable_Name = $('#DownloadsTable_Name');
		$PriorityMenu = $('#PriorityMenu');
		$CategoryMenu = $('#DownloadsCategoryMenu');

		const recordsPerPage = UISettings.read('DownloadsRecordsPerPage', 10);
		$DownloadsRecordsPerPage.val(recordsPerPage);
		$('#DownloadsTable_filter').val('');

		$DownloadsTable.fasttable(
			{
				filterInput: '#DownloadsTable_filter',
				filterClearButton: '#DownloadsTable_clearfilter',
				pagerContainer: '#DownloadsTable_pager',
				infoContainer: '#DownloadsTable_info',
				infoEmpty: '&nbsp;',
				pageSize: recordsPerPage,
				maxPages: UISettings.miniTheme ? 1 : 5,
				pageDots: !UISettings.miniTheme,
				rowSelect: UISettings.rowSelect,
				shortcuts: true,
				fillFieldsCallback: fillFieldsCallback,
				renderCellCallback: renderCellCallback,
				updateInfoCallback: updateInfo,
				dragStartCallback: Refresher.pause,
				dragEndCallback: dragEndCallback,
				dragBlink: 'update'
			});

		$DownloadsTable.on('click', 'a', itemClick);
		$DownloadsTable.on('click', 'td:nth-child(2).dropdown-cell > div:not(.dropdown-disabled)', priorityClick);
		$DownloadsTable.on('click', 'td:nth-child(3).dropdown-cell > div', statusClick);
		$DownloadsTable.on('click', 'td:nth-child(5).dropdown-cell > div:not(.dropdown-disabled)', categoryClick);
		$PriorityMenu.on('click', 'a', priorityMenuClick);
		$CategoryMenu.on('click', 'a', categoryMenuClick);

		DownloadsActionsMenu.init();
	};

	this.applyTheme = function()
	{
		this.redraw(true);
		$DownloadsTable.fasttable('setPageSize', UISettings.read('DownloadsRecordsPerPage', 10),
			UISettings.miniTheme ? 1 : 5, !UISettings.miniTheme);
	};

	this.update = function()
	{
		RPC.call('listgroups', [], groups_loaded, null, { prefer_cached: true });
	};

	this.subscribe = function(sub)
	{
		listGroupsSubs.push(sub);
	};

	function notifyAll(data)
	{
		for (const sub of listGroupsSubs)
		{
			sub.update(data);
		}
	}

	function groups_loaded(_groups, _cached)
	{
		if (!groupsInitialized)
		{
			$('#DownloadsTable_Category').css('width', DownloadsUI.calcCategoryColumnWidth());
			groupsInitialized = true;
		}

		cached = _cached;
		if (!Refresher.isPaused() && !cached)
		{
			groups = Array.isArray(_groups) ? _groups : [];
			Downloads.groups = groups;
			prepare();
		}
		notifyAll(groups);
		RPC.next();
	}

	function prepare()
	{
		for (const group of groups)
		{
			group.postprocess = statusData[group.Status]?.PostProcess ?? false;
		}
	}

	this.redraw = function(force)
	{
		if (cached && !force && lastDownloadRate === Status.status.DownloadRate)
		{
			return;
		}

		if (!Refresher.isPaused())
		{
			redraw_table();
			lastDownloadRate = Status.status.DownloadRate;
		}

		Util.show($DownloadsTabBadge, groups.length > 0);
		Util.show($DownloadsTabBadgeEmpty, groups.length === 0 && UISettings.miniTheme);
		Util.show($DownloadQueueEmpty, groups.length === 0);
	};

	this.resize = function()
	{
		calcProgressLabels();
	};

	/*** TABLE *************************************************************************/

	const SEARCH_FIELDS = Object.freeze(['name', 'status', 'priority', 'category', 'estimated', 'age', 'size', 'remaining']);

	function redraw_table()
	{
		const nowSec = Date.now() / 1000;
		const tz = UISettings.timeZoneCorrection * 3600;

		const data = groups.map((group) => {
			group.name = group.NZBName;
			group.status = DownloadsUI.buildStatusText(group);
			group.priority = DownloadsUI.buildPriorityText(group.MaxPriority);
			group.category = group.Category;
			group.estimated = DownloadsUI.buildEstimated(group);
			group.size = Util.formatSizeMB(group.FileSizeMB, group.FileSizeLo);
			group.sizemb = group.FileSizeMB;
			group.sizegb = group.FileSizeMB / 1024;
			group.left = Util.formatSizeMB(group.RemainingSizeMB - group.PausedSizeMB, group.RemainingSizeLo - group.PausedSizeLo);
			group.leftmb = group.RemainingSizeMB - group.PausedSizeMB;
			group.leftgb = group.leftmb / 1024;
			group.dupe = DownloadsUI.buildDupeText(group.DupeKey, group.DupeScore, group.DupeMode);
			const ageSec = nowSec - (group.MinPostTime + tz);
			group.age = Util.formatAge(group.MinPostTime + tz);
			group.agem = Util.round0(ageSec / 60);
			group.ageh = Util.round0(ageSec / 3600);
			group.aged = Util.round0(ageSec / 86400);

			group._search = SEARCH_FIELDS;

			return { id: group.NZBID, data: group };
		});

		$DownloadsTable.fasttable('update', data);
	}

	function fillFieldsCallback(item)
	{
		const group = item.data;

		let status = DownloadsUI.buildStatus(group);
		let priority = DownloadsUI.buildPriority(group);
		const progresslabel = DownloadsUI.buildProgressLabel(group, nameColumnWidth);
		const progress = DownloadsUI.buildProgress(group, item.data.size, item.data.left, item.data.estimated);
		const dupe = DownloadsUI.buildDupe(group.DupeKey, group.DupeScore, group.DupeMode);

		const age = Date.now() / 1000 - (group.MinPostTime + UISettings.timeZoneCorrection * 3600);
		let propagation = '';
		if (group.ActiveDownloads === 0 && age < parseInt(Options.option('PropagationDelay')) * 60)
		{
			propagation = '<span class="label label-warning" title="Very recent post, temporary delayed (see option PropagationDelay)">delayed</span> ';
		}

		const name = `<a href="#" data-nzbid="${group.NZBID}">${Util.textToHtml(Util.formatNZBName(group.NZBName))}</a>` +
			DownloadsUI.buildEncryptedLabel(group.Parameters);

		const url = group.Kind === 'URL' ? '<span class="label label-info">URL</span> ' : '';

		let health = '';
		if (group.Health < 1000 && (!group.postprocess ||
			(group.Status === 'PP_QUEUED' && group.PostTotalTimeSec === 0)))
		{
			const healthClass = group.Health >= group.CriticalHealth ? 'label-warning' : 'label-important';
			health = ` <span class="label ${healthClass}">health: ${Math.floor(group.Health / 10)}%</span> `;
		}

		const category = group.Category !== '' ? Util.textToHtml(group.Category) : '<span class="none-category">None</span>';
		const backup = DownloadsUI.buildBackupLabel(group);

		if (!UISettings.miniTheme)
		{
			const disabledClass = group.postprocess ? ' class="dropdown-disabled"' : '';
			priority = `<div data-nzbid="${group.NZBID}"${disabledClass}>${priority}</div>`;
			status = `<div data-nzbid="${group.NZBID}">${status}</div>`;
			const wrappedCategory = `<div data-nzbid="${group.NZBID}"${disabledClass}>${category}</div>`;
			const info = `${name} ${url}${dupe}${health}${backup}${propagation}${progresslabel}`;
			item.fields = ['<div class="check img-check"></div>', priority, status, info, wrappedCategory, item.data.age, progress, item.data.estimated];
		}
		else
		{
			const info = '<div class="check img-check"></div><span class="row-title">' +
				name + '</span>' + url + ' ' + (group.MaxPriority === 0 ? '' : priority) +
				' ' + (group.Status === 'QUEUED' ? '' : status) + dupe + health + backup + propagation;
			let miniInfo = info;
			if (group.Category !== '')
			{
				miniInfo += ` <span class="label label-status">${category}</span>`;
			}

			let miniProgress = progress;
			if (progresslabel)
			{
				miniProgress = `<div class="downloads-progresslabel">${progresslabel}</div>${progress}`;
			}
			item.fields = [miniInfo, miniProgress];
		}
	}

	function renderCellCallback(cell, index, item)
	{
		if (index === 1)
		{
			cell.className = 'priority-cell' + (!UISettings.miniTheme ? ' dropdown-cell' : '');
		}
		else if (index === 2 || index === 4)
		{
			cell.className = !UISettings.miniTheme ? 'dropdown-cell dropafter-cell' : '';
		}
		else if (index === 3)
		{
			cell.className = !UISettings.miniTheme ? 'dropafter-cell' : '';
		}
		else if (index === 5)
		{
			cell.className = 'text-right' + (!UISettings.miniTheme ? ' dropafter-cell' : '');
		}
		else if (index >= 6 && index <= 8)
		{
			cell.className = 'text-right';
		}
	}

	this.recordsPerPageChange = function()
	{
		const val = $DownloadsRecordsPerPage.val();
		UISettings.write('DownloadsRecordsPerPage', val);
		$DownloadsTable.fasttable('setPageSize', val);
	};

	function updateInfo(stat)
	{
		updateTabInfo($DownloadsTabBadge, stat);
	}

	function calcProgressLabels()
	{
		const progressLabels = $('.label-inline', $DownloadsTable);

		if (UISettings.miniTheme)
		{
			nameColumnWidth = null;
			progressLabels.css('max-width', '');
			return;
		}

		progressLabels.hide();
		nameColumnWidth = Math.max($DownloadsTable_Name.width(), 50) - 4 * 2;
		progressLabels.css('max-width', nameColumnWidth);
		progressLabels.show();
	}

	this.processShortcut = function(key)
	{
		switch (key)
		{
			case 'A': Upload.addClick(); return true;
			case 'D': case 'Delete': case 'Meta+Backspace': Downloads.deleteClick(); return true;
			case 'E': case 'Enter': Downloads.editClick(); return true;
			case 'U': Downloads.moveClick('up'); return true;
			case 'N': Downloads.moveClick('down'); return true;
			case 'T': Downloads.moveClick('top'); return true;
			case 'B': Downloads.moveClick('bottom'); return true;
			case 'P': Downloads.pauseClick(); return true;
			case 'R': Downloads.resumeClick(); return true;
			case 'M': Downloads.mergeClick(); return true;
		}
		return $DownloadsTable.fasttable('processShortcut', key);
	};

	/*** EDIT ******************************************************/

	function findGroup(nzbid)
	{
		const id = String(nzbid);
		return groups.find(g => String(g.NZBID) === id) ?? null;
	}

	function itemClick(e)
	{
		e.preventDefault();
		e.stopPropagation();
		const nzbid = $(this).attr('data-nzbid');
		const area = $(this).attr('data-area');
		$(this).blur();
		DownloadsEditDialog.showModal(nzbid, groups, area);
	}

	function editCompleted()
	{
		Refresher.update();
		if (notification)
		{
			PopupNotification.show(notification);
			notification = null;
		}
	}

	/*** CHECKMARKS ******************************************************/

	function checkBuildEditIDList(allowPostProcess, allowUrl, allowEmpty)
	{
		const checkedRows = $DownloadsTable.fasttable('checkedRows');
		const checkedEditIDs = [];

		for (const group of groups)
		{
			if (checkedRows[group.NZBID])
			{
				if (group.postprocess && !allowPostProcess)
				{
					PopupNotification.show('#Notif_Downloads_CheckPostProcess');
					return null;
				}
				if (group.Kind === 'URL' && !allowUrl)
				{
					PopupNotification.show('#Notif_Downloads_CheckURL');
					return null;
				}

				checkedEditIDs.push(group.NZBID);
			}
		}

		if (checkedEditIDs.length === 0 && !allowEmpty)
		{
			PopupNotification.show('#Notif_Downloads_Select');
			return null;
		}

		return checkedEditIDs;
	}

	function buildContextIdList(group)
	{
		const editIds = checkBuildEditIDList(true, true, true);
		if (!editIds || !editIds.includes(group.NZBID))
		{
			return [group.NZBID];
		}
		return editIds;
	}
	this.buildContextIdList = buildContextIdList;

	/*** TOOLBAR: SELECTED ITEMS ******************************************************/

	this.editClick = function()
	{
		const checkedEditIDs = checkBuildEditIDList(false, true);
		if (!checkedEditIDs)
		{
			return;
		}

		if (checkedEditIDs.length === 1)
		{
			DownloadsEditDialog.showModal(checkedEditIDs[0], groups);
		}
		else
		{
			DownloadsMultiDialog.showModal(checkedEditIDs, groups);
		}
	};

	this.mergeClick = function()
	{
		const checkedEditIDs = checkBuildEditIDList(false, false);
		if (!checkedEditIDs)
		{
			return;
		}

		if (checkedEditIDs.length < 2)
		{
			PopupNotification.show('#Notif_Downloads_SelectMulti');
			return;
		}

		DownloadsMergeDialog.showModal(checkedEditIDs, groups);
	};

	this.pauseClick = function()
	{
		const checkedEditIDs = checkBuildEditIDList(false, false);
		if (!checkedEditIDs)
		{
			return;
		}
		notification = '#Notif_Downloads_Paused';
		RPC.call('editqueue', ['GroupPause', '', checkedEditIDs], editCompleted);
	};

	this.resumeClick = function()
	{
		const checkedEditIDs = checkBuildEditIDList(false, false);
		if (!checkedEditIDs)
		{
			return;
		}
		notification = '#Notif_Downloads_Resumed';
		RPC.call('editqueue', ['GroupResume', '', checkedEditIDs], function()
		{
			if (Options.option('ParCheck') === 'force')
			{
				editCompleted();
			}
			else
			{
				RPC.call('editqueue', ['GroupPauseExtraPars', '', checkedEditIDs], editCompleted);
			}
		});
	};

	this.deleteClick = function()
	{
		const checkedRows = $DownloadsTable.fasttable('checkedRows');
		const downloadIDs = [];
		const postprocessIDs = [];
		let hasNzb = false;
		let hasUrl = false;

		for (const group of groups)
		{
			if (checkedRows[group.NZBID])
			{
				if (group.postprocess)
				{
					postprocessIDs.push(group.NZBID);
				}
				downloadIDs.push(group.NZBID);
				hasNzb = hasNzb || group.Kind === 'NZB';
				hasUrl = hasUrl || group.Kind === 'URL';
			}
		}

		if (downloadIDs.length === 0 && postprocessIDs.length === 0)
		{
			PopupNotification.show('#Notif_Downloads_Select');
			return;
		}

		notification = '#Notif_Downloads_Deleted';

		const deletePosts = function()
		{
			if (postprocessIDs.length > 0)
			{
				RPC.call('editqueue', ['PostDelete', '', postprocessIDs], editCompleted);
			}
			else
			{
				editCompleted();
			}
		};

		const deleteGroups = function(command)
		{
			if (downloadIDs.length > 0)
			{
				RPC.call('editqueue', [command, '', downloadIDs], deletePosts);
			}
			else
			{
				deletePosts();
			}
		};

		DownloadsUI.deleteConfirm(deleteGroups, true, hasNzb, hasUrl, downloadIDs.length);
	};

	this.moveClick = function(action)
	{
		const checkedEditIDs = checkBuildEditIDList(true, true);
		if (!checkedEditIDs)
		{
			return;
		}

		let EditAction = '';
		let EditOffset = 0;
		switch (action)
		{
			case 'top':
				EditAction = 'GroupMoveTop';
				checkedEditIDs.reverse();
				break;
			case 'bottom':
				EditAction = 'GroupMoveBottom';
				break;
			case 'up':
				EditAction = 'GroupMoveOffset';
				EditOffset = -1;
				break;
			case 'down':
				EditAction = 'GroupMoveOffset';
				EditOffset = 1;
				checkedEditIDs.reverse();
				break;
		}

		notification = '';
		RPC.call('editqueue', [EditAction, '' + EditOffset, checkedEditIDs], editCompleted);
	};

	this.sort = function(order, e)
	{
		e.preventDefault();
		e.stopPropagation();
		const checkedEditIDs = checkBuildEditIDList(true, true, true);
		notification = '#Notif_Downloads_Sorted';
		RPC.call('editqueue', ['GroupSort', order, checkedEditIDs], editCompleted);
	};

	function dragEndCallback(info)
	{
		if (info)
		{
			RPC.call('editqueue', [info.direction === 'after' ? 'GroupMoveAfter' : 'GroupMoveBefore',
				'' + info.position, info.ids], function(){ Refresher.resume(true); });
		}
		else
		{
			Refresher.resume();
		}
	}

	function priorityClick(e)
	{
		e.preventDefault();
		e.stopPropagation();
		const group = findGroup($(this).attr('data-nzbid'));
		if (!group) return;
		const editIds = buildContextIdList(group);
		$PriorityMenu.data('nzbids', editIds);
		DownloadsUI.updateContextWarning($PriorityMenu, editIds);
		$('i', $PriorityMenu).css('visibility', 'hidden');
		$('li[data=' + group.MaxPriority + '] i', $PriorityMenu).css('visibility', 'visible');
		Frontend.showPopupMenu($PriorityMenu, 'left',
			{ left: $(this).offset().left - 30, top: $(this).offset().top,
				width: $(this).width() + 30, height: $(this).outerHeight() - 2});
	}

	function priorityMenuClick(e)
	{
		e.preventDefault();
		const priority = $(this).parent().attr('data');
		const nzbids = $PriorityMenu.data('nzbids');
		notification = '#Notif_Downloads_Changed';
		RPC.call('editqueue', ['GroupSetPriority', '' + priority, nzbids], editCompleted);
	}

	function statusClick(e)
	{
		e.preventDefault();
		e.stopPropagation();
		const group = findGroup($(this).attr('data-nzbid'));
		if (!group) return;

		DownloadsActionsMenu.showPopupMenu(group, 'left',
			{ left: $(this).offset().left - 30, top: $(this).offset().top,
				width: $(this).width() + 30, height: $(this).outerHeight() - 2 },
			function(_notification) { notification = _notification; },
			editCompleted);
	}

	function categoryClick(e)
	{
		e.preventDefault();
		e.stopPropagation();
		DownloadsUI.fillCategoryMenu($CategoryMenu);
		const group = findGroup($(this).attr('data-nzbid'));

		if (!group || group.postprocess)
		{
			return;
		}

		const editIds = buildContextIdList(group);
		$CategoryMenu.data('nzbids', editIds);
		DownloadsUI.updateContextWarning($CategoryMenu, editIds);
		$('i', $CategoryMenu).text('');
		$('li[data="' + group.Category + '"] i', $CategoryMenu).text('done');

		Frontend.showPopupMenu($CategoryMenu, 'left',
			{ left: $(this).offset().left - 30, top: $(this).offset().top,
				width: $(this).width() + 30, height: $(this).outerHeight() - 2 });
	}

	function categoryMenuClick(e)
	{
		e.preventDefault();
		const category = $(this).parent().attr('data');
		const nzbids = $CategoryMenu.data('nzbids');
		notification = '#Notif_Downloads_Changed';
		RPC.call('editqueue', ['GroupApplyCategory', category, nzbids], editCompleted);
	}

}(jQuery));


/*** DOWNLOADS ACTION MENU *************************************************************************/

var DownloadsActionsMenu = (new function($)
{
	'use strict';

	let $ActionsMenu;
	let curGroup;
	let beforeCallback;
	let completedCallback;
	let editIds;

	this.init = function()
	{
		$ActionsMenu = $('#DownloadsActionsMenu');
		$('#DownloadsActions_Pause').click(itemPause);
		$('#DownloadsActions_Resume').click(itemResume);
		$('#DownloadsActions_Delete').click(itemDelete);
		$('#DownloadsActions_CancelPP').click(itemCancelPP);
	};

	this.showPopupMenu = function(group, anchor, rect, before, completed)
	{
		curGroup = group;
		beforeCallback = before;
		completedCallback = completed;
		editIds = Downloads.buildContextIdList(group);

		Util.show('#DownloadsActions_CancelPP', group.postprocess);
		Util.show('#DownloadsActions_Delete', !group.postprocess);
		Util.show('#DownloadsActions_Pause', group.Kind === 'NZB' && !group.postprocess);
		Util.show('#DownloadsActions_Resume', false);
		DownloadsUI.updateContextWarning($ActionsMenu, editIds);

		if (!group.postprocess &&
			(group.RemainingSizeHi === group.PausedSizeHi &&
				group.RemainingSizeLo === group.PausedSizeLo &&
				group.Kind === 'NZB'))
		{
			$('#DownloadsActions_Resume').show();
			$('#DownloadsActions_Pause').hide();
		}

		DownloadsUI.buildDNZBLinks(group.Parameters, 'DownloadsActions_DNZB');

		Frontend.showPopupMenu($ActionsMenu, anchor, rect);
	};

	function itemPause(e)
	{
		e.preventDefault();
		beforeCallback('#Notif_Downloads_Paused');
		RPC.call('editqueue', ['GroupPause', '', editIds], completedCallback);
	}

	function itemResume(e)
	{
		e.preventDefault();
		beforeCallback('#Notif_Downloads_Resumed');
		RPC.call('editqueue', ['GroupResume', '', editIds], function()
		{
			if (Options.option('ParCheck') === 'force')
			{
				completedCallback();
			}
			else
			{
				RPC.call('editqueue', ['GroupPauseExtraPars', '', editIds], completedCallback);
			}
		});
	}

	function itemDelete(e)
	{
		e.preventDefault();
		DownloadsUI.deleteConfirm(doItemDelete, false, curGroup.Kind === 'NZB', curGroup.Kind === 'URL');
	}

	function doItemDelete(command)
	{
		beforeCallback('#Notif_Downloads_Deleted');
		RPC.call('editqueue', [command, '', editIds], completedCallback);
	}

	function itemCancelPP(e)
	{
		e.preventDefault();
		beforeCallback('#Notif_Downloads_PostCanceled');
		RPC.call('editqueue', ['PostDelete', '', editIds], completedCallback);
	}

}(jQuery));


/*** FUNCTIONS FOR HTML GENERATION (also used from other modules) *****************************/

var DownloadsUI = (new function($)
{
	'use strict';

	let categoryColumnWidth = null;
	let dupeCheck = null;
	let minLevel = null;

	this.fillPriorityCombo = function(combo)
	{
		combo.empty();
		combo.append('<option value="900">force</option>');
		combo.append('<option value="100">very high</option>');
		combo.append('<option value="50">high</option>');
		combo.append('<option value="0">normal</option>');
		combo.append('<option value="-50">low</option>');
		combo.append('<option value="-100">very low</option>');
	};

	this.fillCategoryCombo = function(combo)
	{
		combo.empty();
		combo.append('<option></option>');

		for (const cat of Options.categories)
		{
			combo.append($('<option></option>').text(cat));
		}
	};

	this.fillCategoryMenu = function(menu)
	{
		if (menu.data('initialized'))
		{
			return;
		}
		const templ = $('li:last-child', menu);
		for (const cat of Options.categories)
		{
			const item = templ.clone().show();
			$('td:last-child', item).text(cat);
			item.attr('data', cat);
			menu.append(item);
		}
		menu.data('initialized', true);
	};

	this.buildStatusText = function(group)
	{
		const entry = Downloads.statusData[group.Status];
		return entry?.Text ?? `INTERNAL_ERROR (${group.Status})`;
	};

	this.buildStatus = function(group)
	{
		const entry = Downloads.statusData[group.Status];
		const statusText = entry?.Text ?? `INTERNAL_ERROR (${group.Status})`;
		let badgeClass = '';

		if (group.postprocess && group.Status !== 'PP_QUEUED' && group.Status !== 'QS_QUEUED')
		{
			badgeClass = Status.status.PostPaused && group.MinPriority < 900 ? 'label-warning' : 'label-success';
		}
		else if (group.Status === 'DOWNLOADING' || group.Status === 'FETCHING' || group.Status === 'QS_EXECUTING')
		{
			badgeClass = 'label-success';
		}
		else if (group.Status === 'PAUSED')
		{
			badgeClass = 'label-warning';
		}
		else if (!entry)
		{
			badgeClass = 'label-important';
		}

		return `<span class="label label-status ${badgeClass}">${statusText}</span>`;
	};

	this.buildProgress = function(group, totalsize, remaining, estimated)
	{
		let kind;
		if (group.Status === 'DOWNLOADING' ||
			(group.postprocess && !(Status.status.PostPaused && group.MinPriority < 900)))
		{
			kind = 'progress-success';
		}
		else if (group.Status === 'PAUSED' ||
			(group.postprocess && (Status.status.PostPaused && group.MinPriority < 900)))
		{
			kind = 'progress-warning';
		}
		else
		{
			kind = 'progress-none';
		}

		const totalMB = group.FileSizeMB - group.PausedSizeMB;
		const remainingMB = group.RemainingSizeMB - group.PausedSizeMB;
		let percent = totalMB > 0 ? Math.round((totalMB - remainingMB) / totalMB * 100) : 0;

		if (group.postprocess)
		{
			totalsize = '';
			remaining = '';
			percent = Math.round(group.PostStageProgress / 10);
		}

		if (group.Kind === 'URL')
		{
			totalsize = '';
			remaining = '';
		}

		if (!UISettings.miniTheme)
		{
			return `<div class="progress-block">` +
				`<div class="progress progress-striped ${kind}">` +
				`<div class="bar" style="width:${percent}%;"></div>` +
				`</div>` +
				`<div class="bar-text-left">${totalsize}</div>` +
				`<div class="bar-text-right">${remaining}</div>` +
				`</div>`;
		}
		else
		{
			return `<div class="progress-block">` +
				`<div class="progress progress-striped ${kind}">` +
				`<div class="bar" style="width:${percent}%;"></div>` +
				`</div>` +
				`<div class="bar-text-left">${totalsize !== '' ? 'total ' : ''}${totalsize}</div>` +
				`<div class="bar-text-center">${estimated !== '' ? '[' + estimated + ']' : ''}</div>` +
				`<div class="bar-text-right">${remaining}${remaining !== '' ? ' left' : ''}</div>` +
				`</div>`;
		}
	};

	this.buildEstimated = function(group)
	{
		if (group.postprocess)
		{
			if (group.PostStageProgress > 0)
			{
				return Util.formatTimeLeft(group.PostStageTimeSec / group.PostStageProgress * (1000 - group.PostStageProgress));
			}
		}
		else if (group.Status !== 'PAUSED' && Status.status.DownloadRate > 0)
		{
			return Util.formatTimeLeft((group.RemainingSizeMB - group.PausedSizeMB) * 1024 / (Status.status.DownloadRate / 1024));
		}

		return '';
	};

	this.buildProgressLabel = function(group, maxWidth)
	{
		let text = '';
		if (group.postprocess && !(Status.status.PostPaused && group.MinPriority < 900))
		{
			switch (group.Status)
			{
				case 'LOADING_PARS':
				case 'VERIFYING_SOURCES':
				case 'VERIFYING_REPAIRED':
				case 'UNPACKING':
				case 'RENAMING':
				case 'EXECUTING_SCRIPT':
					text = group.PostInfoText;
					break;
			}
		}

		return text !== '' ? ` <span class="label label-success label-inline" style="max-width:${maxWidth}px">${text}</span>` : '';
	};

	this.buildPriorityText = function(priority)
	{
		priority = parseInt(priority, 10);
		switch (priority)
		{
			case 0: return '';
			case 900: return 'force priority';
			case 100: return 'very high priority';
			case 50: return 'high priority';
			case -50: return 'low priority';
			case -100: return 'very low priority';
			default: return 'priority: ' + priority;
		}
	};

	this.buildPriority = function(group)
	{
		const priority = parseInt(group.MaxPriority, 10);
		let text;

		if (priority >= 900) text = ' <div class="icon-circle-red" title="Force priority"></div>';
		else if (priority > 50) text = ' <div class="icon-ring-fill-red" title="Very high priority"></div>';
		else if (priority > 0) text = ' <div class="icon-ring-red" title="High priority"></div>';
		else if (priority === 0) text = ' <div class="icon-ring-ltgrey" title="Normal priority"></div>';
		else if (priority >= -50) text = ' <div class="icon-ring-blue" title="Low priority"></div>';
		else text = ' <div class="icon-ring-fill-blue" title="Very low priority"></div>';

		if (![900, 100, 50, 0, -50, -100].includes(priority))
		{
			text = text.replace('priority', `priority (${priority})`);
		}

		return text;
	};

	this.buildEncryptedLabel = function(parameters)
	{
		if (!parameters) return '';

		let encryptedPassword = '';

		for (const param of parameters)
		{
			if (param['Name'].toLowerCase() === '*unpack:password')
			{
				encryptedPassword = param['Value'];
				break;
			}
		}
		return encryptedPassword !== '' ?
			` <span class="label label-info" title="${Util.textToAttr(encryptedPassword)}">encrypted</span>` : '';
	};

	this.buildBackupLabel = function(group)
	{
		const backupPercent = calcBackupPercent(group);
		if (backupPercent > 0)
		{
			const formatted = backupPercent < 10 ? Util.round1(backupPercent) : Util.round0(backupPercent);
			return ` <a href="#" data-nzbid="${group.NZBID}" data-area="backup" class="badge-link"><span class="label label-warning" title="with backup news servers">backup: ${formatted}%</span></a> `;
		}
		return '';
	};

	function calcBackupPercent(group)
	{
		const downloadedArticles = group.SuccessArticles + group.FailedArticles;
		if (downloadedArticles === 0)
		{
			return 0;
		}

		if (minLevel === null)
		{
			for (const server of Status.status.NewsServers)
			{
				const level = parseInt(Options.option('Server' + server.ID + '.Level'));
				if (minLevel === null || minLevel > level)
				{
					minLevel = level;
				}
			}
		}

		let backupArticles = 0;
		for (const stat of group.ServerStats)
		{
			const level = parseInt(Options.option('Server' + stat.ServerID + '.Level'));
			if (level > minLevel && stat.SuccessArticles > 0)
			{
				backupArticles += stat.SuccessArticles;
			}
		}

		if (backupArticles > 0)
		{
			return backupArticles * 100.0 / downloadedArticles;
		}
		return 0;
	}

	const DUPE_KEY_REPLACEMENTS = [
		['rageid=', ''],
		['tvdbid=', ''],
		['tvmazeid=', ''],
		['imdb=', ''],
		['series=', ''],
		['nzb=', '#']
	];

	function formatDupeText(dupeKey)
	{
		for (const [search, replacement] of DUPE_KEY_REPLACEMENTS)
		{
			dupeKey = dupeKey.replace(search, replacement);
		}
		return dupeKey === '' ? 'title' : dupeKey;
	}

	this.buildDupeText = function(dupeKey, dupeScore, dupeMode)
	{
		if (dupeCheck === null)
		{
			dupeCheck = Options.option('DupeCheck') === 'yes';
		}

		if (dupeCheck && dupeKey && UISettings.dupeBadges)
		{
			return formatDupeText(dupeKey);
		}
		return '';
	};

	this.buildDupe = function(dupeKey, dupeScore, dupeMode)
	{
		if (dupeCheck === null)
		{
			dupeCheck = Options.option('DupeCheck') === 'yes';
		}

		if (dupeCheck && dupeKey && UISettings.dupeBadges)
		{
			const labelClass = dupeMode === 'FORCE' ? ' label-important' : '';
			const scoreInfo = dupeScore !== 0 ? `; score: ${dupeScore}` : '';
			const modeInfo = dupeMode !== 'SCORE' ? `; mode: ${dupeMode.toLowerCase()}` : '';
			return ` <span class="label${labelClass}" title="Duplicate key: ${Util.textToAttr(dupeKey)}${scoreInfo}${modeInfo}">${formatDupeText(dupeKey)}</span> `;
		}
		return '';
	};

	this.resetCategoryColumnWidth = function()
	{
		categoryColumnWidth = null;
	};

	this.calcCategoryColumnWidth = function()
	{
		if (categoryColumnWidth !== null)
		{
			return categoryColumnWidth;
		}

		let max = 60;

		const helper = document.createElement('span');
		helper.style.cssText = 'position:absolute;white-space:nowrap;visibility:hidden';

		const header = document.getElementById('DownloadsTable_Category');
		if (header)
		{
			const cs = getComputedStyle(header);
			helper.style.font = cs.font;
		}

		document.body.appendChild(helper);

		for (let i = 1; ; i++)
		{
			const opt = Options.option(`Category${i}.Name`);
			if (!opt) break;
			helper.textContent = opt;
			max = Math.max(max, helper.getBoundingClientRect().width);
		}

		helper.remove();

		categoryColumnWidth = Math.ceil(max + 8) + 'px';
		return categoryColumnWidth;
	};

	this.buildDNZBLinks = function(parameters, prefix)
	{
		$('.' + prefix).hide();
		let hasItems = false;

		if (!parameters) {
			Util.show('#' + prefix + '_Section', false);
			return;
		}

		for (const param of parameters)
		{
			if (param.Name.startsWith('*DNZB:'))
			{
				const linkName = param.Name.slice(6);
				const $paramLink = $('#' + prefix + '_' + linkName);
				if ($paramLink.length > 0)
				{
					$paramLink.attr('href', param.Value);
					$paramLink.show();
					hasItems = true;
				}
			}
		}

		Util.show('#' + prefix + '_Section', hasItems);
	};

	this.deleteConfirm = function(actionCallback, multi, hasNzb, hasUrl, selCount)
	{
		const dupeCheckOpt = Options.option('DupeCheck') === 'yes';
		const history = Options.option('KeepHistory') !== '0';
		let dialog = null;

		function init(_dialog)
		{
			dialog = _dialog;

			if (!multi)
			{
				const html = $('#ConfirmDialog_Text').html();
				$('#ConfirmDialog_Text').html(html.replace(/downloads/g, 'download'));
			}

			$('#DownloadsDeleteConfirmDialog_DeletePark', dialog).prop('checked', true);
			$('#DownloadsDeleteConfirmDialog_DeleteDirect', dialog).prop('checked', false);
			$('#DownloadsDeleteConfirmDialog_DeleteDupe', dialog).prop('checked', false);
			$('#DownloadsDeleteConfirmDialog_DeleteFinal', dialog).prop('checked', false);
			Util.show($('#DownloadsDeleteConfirmDialog_Options', dialog), history);
			Util.show($('#DownloadsDeleteConfirmDialog_Simple', dialog), !history);
			Util.show($('#DownloadsDeleteConfirmDialog_DeleteDupe,#DownloadsDeleteConfirmDialog_DeleteDupeLabel', dialog), dupeCheckOpt && hasNzb);
			Util.show('#ConfirmDialog_Help', history && dupeCheckOpt && hasNzb);
		}

		function action()
		{
			const deletePark = $('#DownloadsDeleteConfirmDialog_DeletePark', dialog).is(':checked');
			const deleteDirect = $('#DownloadsDeleteConfirmDialog_DeleteDirect', dialog).is(':checked');
			const deleteDupe = $('#DownloadsDeleteConfirmDialog_DeleteDupe', dialog).is(':checked');
			const command = deletePark ? 'GroupParkDelete' : (deleteDirect ? 'GroupDelete' : (deleteDupe ? 'GroupDupeDelete' : 'GroupFinalDelete'));
			actionCallback(command);
		}

		ConfirmDialog.showModal('DownloadsDeleteConfirmDialog', action, init, selCount);
	};

	this.updateContextWarning = function(menu, editIds)
	{
		const warning = $('.dropdown-warning', $(menu));
		Util.show(warning, editIds.length > 1);
		warning.text(editIds.length + ' records selected');
	};
}(jQuery));
