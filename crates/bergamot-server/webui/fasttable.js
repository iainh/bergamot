/*
 *
 * Copyright (C) 2012-2019 Andrey Prygunkov <hugbug@users.sourceforge.net>
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
 * along with this program.  If not, see <http://www.gnu.org/licenses/>.
 */

/*
 * In this module:
 *   HTML tables with:
 *     1) very fast content updates via rAF-coalesced rendering;
 *     2) automatic pagination;
 *     3) search/filtering;
 *     4) drag and drop via Pointer Events.
 *
 * Updates are coalesced to at most one render per animation frame.
 * DOM updates are deferred during active user interactions (drag, selection)
 * to prevent click/pointer events from being lost due to DOM mutations.
 * Cached DOM references and minimal cell patching avoid unnecessary layout work.
 */

(function($) {

	'use strict';

	$.fn.fasttable = function(method)
	{
		if (methods[method])
		{
			return methods[method].apply( this, Array.prototype.slice.call( arguments, 1 ));
		}
		else if ( typeof method === 'object' || ! method )
		{
			return methods.init.apply( this, arguments );
		}
		else
		{
			$.error( 'Method ' +  method + ' does not exist on jQuery.fasttable' );
		}
	};

	var methods =
	{
		defaults : function()
		{
			return defaults;
		},

		init : function(options)
		{
			return this.each(function()
			{
				var $this = $(this);
				var data = $this.data('fasttable');

				if (!data)
				{
					var config = {};
					config = $.extend(config, defaults, options);

					config.filterInput = $(config.filterInput);
					config.filterClearButton = $(config.filterClearButton);
					config.pagerContainer = $(config.pagerContainer);
					config.infoContainer = $(config.infoContainer);
					config.dragBox = $(config.dragBox);
					config.dragContent = $(config.dragContent);
					config.dragBadge = $(config.dragBadge);
					config.selector = $('th.table-selector', $this);

					var searcher = new FastSearcher();

					var timer;
					var filterInputEl = config.filterInput[0];

					if (filterInputEl)
					{
						filterInputEl.addEventListener('input', function()
						{
							var inputBox = this;
							clearTimeout(timer);
							timer = setTimeout(function()
							{
								var value = inputBox.value.trim();
								var data = $this.data('fasttable');
								if (!data) return;
								if (value !== data.lastFilter)
								{
									applyFilter(data, value);
								}
							}, 500);
						});

						filterInputEl.addEventListener('keydown', function(e)
						{
							if (e.key === 'Enter')
							{
								clearTimeout(timer);
								var value = this.value.trim();
								var data = $this.data('fasttable');
								if (!data) return;
								applyFilter(data, value);
							}
						});
					}

					config.filterClearButton.click(function()
					{
						var data = $this.data('fasttable');
						data.config.filterInput.val('');
						applyFilter(data, '');
					});

					config.pagerContainer.on('click', 'li', function (e)
					{
						e.preventDefault();
						var data = $this.data('fasttable');
						var pageNum = this.textContent;
						if (pageNum.indexOf('Prev') > -1)
						{
							data.curPage--;
						}
						else if (pageNum.indexOf('Next') > -1)
						{
							data.curPage++;
						}
						else if (isNaN(parseInt(pageNum)))
						{
							return;
						}
						else
						{
							data.curPage = parseInt(pageNum);
						}
						refresh(data);
					});

					var data = {
						target: $this,
						tableEl: $this[0],
						config: config,
						content: [],
						pageSize: parseInt(config.pageSize),
						maxPages: parseInt(config.maxPages),
						pageDots: Util.parseBool(config.pageDots),
						curPage: 1,
						checkedRows: {},
						checkedCount: 0,
						lastClickedRowID: null,
						searcher: searcher,
						rafHandle: 0,
						pendingContent: null,
						pendingBlink: false,
						isInteracting: false,
						rowCache: new Map()
					};

					initDragDrop(data);

					$this.on('click.fasttable', 'thead > tr', function(e) { titleCheckClick(data, e); });
					$this.on('click.fasttable', 'tbody > tr', function(e) { itemCheckClick(data, e); });

					$this.data('fasttable', data);
				}
			});
		},

		destroy : function()
		{
			return this.each(function()
			{
				var $this = $(this);
				var data = $this.data('fasttable');
				if (!data) return;

				if (data.rafHandle)
				{
					cancelAnimationFrame(data.rafHandle);
				}
				if (data.blinkTimer)
				{
					clearTimeout(data.blinkTimer);
				}
				if (data.scrollTimer)
				{
					clearTimeout(data.scrollTimer);
				}
				if (data.isInteracting)
				{
					pointerUp(data, null);
				}
				if (data.onPointerDown)
				{
					data.tableEl.removeEventListener('pointerdown', data.onPointerDown);
				}
				$this.off('.fasttable');
				$this.removeData('fasttable');
			});
		},

		update : updateContent,

		setPageSize : setPageSize,

		setCurPage : setCurPage,

		applyFilter : function(filter)
		{
			applyFilter($(this).data('fasttable'), filter);
		},

		filteredContent : function()
		{
			return $(this).data('fasttable').filteredContent;
		},

		availableContent : function()
		{
			return $(this).data('fasttable').availableContent;
		},

		checkedRows : function()
		{
			return $(this).data('fasttable').checkedRows;
		},

		checkedCount : function()
		{
			return $(this).data('fasttable').checkedCount;
		},

		pageCheckedCount : function()
		{
			return $(this).data('fasttable').pageCheckedCount;
		},

		checkRow : function(id, checked)
		{
			checkRow($(this).data('fasttable'), id, checked);
		},

		processShortcut : function(key)
		{
			return processShortcut($(this).data('fasttable'), key);
		},
	};

	//*************** RAF-COALESCED RENDERING

	function scheduleRender(data, doBlink)
	{
		if (doBlink)
		{
			data.pendingBlink = true;
		}
		if (data.rafHandle)
		{
			return;
		}
		data.rafHandle = requestAnimationFrame(function()
		{
			data.rafHandle = 0;
			if (data.isInteracting)
			{
				return;
			}
			var content = data.pendingContent;
			if (content !== null)
			{
				data.content = content;
				data.pendingContent = null;
			}
			refresh(data);
			if (data.pendingBlink)
			{
				data.pendingBlink = false;
				blinkMovedRecords(data);
			}
		});
	}

	function updateContent(content)
	{
		var data = $(this).data('fasttable');
		if (content)
		{
			data.pendingContent = content;
		}
		scheduleRender(data, true);
	}

	function applyFilter(data, filter)
	{
		data.lastFilter = filter;
		if (data.content)
		{
			data.curPage = 1;
			data.hasFilter = filter !== '';
			data.searcher.compile(filter);
			refresh(data);
		}
		if (filter !== '' && data.config.filterInputCallback)
		{
			data.config.filterInputCallback(filter);
		}
		if (filter === '' && data.config.filterClearCallback)
		{
			data.config.filterClearCallback();
		}
	}

	function refresh(data)
	{
		refilter(data);
		validateChecks(data);
		updatePager(data);
		updateInfo(data);
		updateSelector(data);
		updateTable(data);
	}

	function refilter(data)
	{
		data.availableContent = [];
		data.filteredContent = [];
		for (var i = 0; i < data.content.length; i++)
		{
			var item = data.content[i];
			if (data.hasFilter && item.search === undefined && data.config.fillSearchCallback)
			{
				data.config.fillSearchCallback(item);
			}

			if (!data.hasFilter || data.searcher.exec(item.data))
			{
				data.availableContent.push(item);
				if (!data.config.filterCallback || data.config.filterCallback(item))
				{
					data.filteredContent.push(item);
				}
			}
		}
	}

	//*************** TABLE UPDATE WITH CACHED DOM REFERENCES

	function updateTable(data)
	{
		var tableEl = data.tableEl;
		var tbody = tableEl.tBodies[0];
		if (!tbody) return;

		var headerRows = tableEl.tHead ? tableEl.tHead.rows.length : 0;
		var pageContent = data.pageContent;
		var oldRows = tbody.rows;
		var oldRowCount = oldRows.length;
		var newRowCount = pageContent.length;

		for (var i = 0; i < newRowCount; i++)
		{
			var item = pageContent[i];

			if (!item.fields)
			{
				if (data.config.fillFieldsCallback)
				{
					data.config.fillFieldsCallback(item);
				}
				else
				{
					item.fields = [];
				}
			}

			var wantClass = data.checkedRows[item.id] ? 'checked' : '';

			if (i < oldRowCount)
			{
				var oldRow = oldRows[i];
				oldRow.fasttableID = item.id;

				if (data.config.renderRowCallback)
				{
					data.config.renderRowCallback(oldRow, item);
				}
				else if (oldRow.className !== wantClass)
				{
					oldRow.className = wantClass;
				}

				var oldCells = oldRow.cells;
				for (var j = 0; j < item.fields.length; j++)
				{
					if (j < oldCells.length)
					{
						var oldCell = oldCells[j];
						var newHtml = item.fields[j];
						if (oldCell.innerHTML !== newHtml)
						{
							oldCell.innerHTML = newHtml;
						}
						if (data.config.renderCellCallback)
						{
							data.config.renderCellCallback(oldCell, j, item);
						}
					}
					else
					{
						var cell = oldRow.insertCell(oldRow.cells.length);
						cell.innerHTML = item.fields[j];
						if (data.config.renderCellCallback)
						{
							data.config.renderCellCallback(cell, j, item);
						}
					}
				}
				while (oldRow.cells.length > item.fields.length)
				{
					oldRow.deleteCell(oldRow.cells.length - 1);
				}
			}
			else
			{
				var row = tbody.insertRow(tbody.rows.length);
				row.fasttableID = item.id;
				if (wantClass)
				{
					row.className = wantClass;
				}
				if (data.config.renderRowCallback)
				{
					data.config.renderRowCallback(row, item);
				}

				for (var j = 0; j < item.fields.length; j++)
				{
					var cell = row.insertCell(row.cells.length);
					cell.innerHTML = item.fields[j];
					if (data.config.renderCellCallback)
					{
						data.config.renderCellCallback(cell, j, item);
					}
				}
			}
		}

		while (tbody.rows.length > newRowCount)
		{
			tbody.deleteRow(tbody.rows.length - 1);
		}

		titleCheckRedraw(data);

		if (data.config.renderTableCallback)
		{
			data.config.renderTableCallback(data.tableEl);
		}
	}

	function updatePager(data)
	{
		data.pageCount = Math.ceil(data.filteredContent.length / data.pageSize);
		if (data.curPage < 1)
		{
			data.curPage = 1;
		}
		if (data.curPage > data.pageCount)
		{
			data.curPage = data.pageCount;
		}

		var startIndex = (data.curPage - 1) * data.pageSize;
		data.pageContent = data.filteredContent.slice(startIndex, startIndex + data.pageSize);

		var pagerObj = data.config.pagerContainer;
		var pagerHtml = buildPagerHtml(data);

		var oldPager = pagerObj[0];
		if (!oldPager) return;

		if (oldPager.tagName !== 'UL')
		{
			pagerObj.html(pagerHtml);
			return;
		}

		var newPager = document.createElement('div');
		newPager.innerHTML = pagerHtml;
		newPager = newPager.firstElementChild;

		updatePagerContent(data, oldPager, newPager);
	}

	function buildPagerHtml(data)
	{
		var iListLength = data.maxPages;
		var iStart, iEnd, iHalf = Math.floor(iListLength/2);

		if (data.pageCount < iListLength)
		{
			iStart = 1;
			iEnd = data.pageCount;
		}
		else if (data.curPage -1 <= iHalf)
		{
			iStart = 1;
			iEnd = iListLength;
		}
		else if (data.curPage - 1 >= (data.pageCount-iHalf))
		{
			iStart = data.pageCount - iListLength + 1;
			iEnd = data.pageCount;
		}
		else
		{
			iStart = data.curPage - 1 - iHalf + 1;
			iEnd = iStart + iListLength - 1;
		}

		var pager = '<ul>';
		pager += '<li' + (data.curPage === 1 || data.curPage === 0 ? ' class="disabled"' : '') +
			'><a href="#" title="Previous page' + (data.config.shortcuts ? ' [Left]' : '') + '">&larr; Prev</a></li>';

		if (iStart > 1)
		{
			pager += '<li><a href="#"' + (data.config.shortcuts ? ' title="First page [Shift+Left]"' : '') + '>1</a></li>';
			if (iStart > 2 && data.pageDots)
			{
				pager += '<li class="disabled"><a href="#">&#133;</a></li>';
			}
		}

		for (var j=iStart; j<=iEnd; j++)
		{
			pager += '<li' + ((j===data.curPage) ? ' class="active"' : '') +
				'><a href="#"' +
				(data.config.shortcuts && j === 1 ? ' title="First page [Shift+Left]"' :
				 data.config.shortcuts && j === data.pageCount ? ' title="Last page [Shift+Right]"' : '') +
				'>' + j + '</a></li>';
		}

		if (iEnd != data.pageCount)
		{
			if (iEnd < data.pageCount - 1 && data.pageDots)
			{
				pager += '<li class="disabled"><a href="#">&#133;</a></li>';
			}
			pager += '<li><a href="#"' + (data.config.shortcuts ? ' title="Last page [Shift+Right]"' : '') + '>' + data.pageCount + '</a></li>';
		}

		pager += '<li' + (data.curPage === data.pageCount || data.pageCount === 0 ? ' class="disabled"' : '') +
			'><a href="#" title="Next page' + (data.config.shortcuts ? ' [Right]' : '') + '">Next &rarr;</a></li>';
		pager += '</ul>';

		return pager;
	}

	function updatePagerContent(data, oldPager, newPager)
	{
		if (!newPager) return;

		var oldLIs = oldPager.getElementsByTagName('li');
		var newLIs = newPager.getElementsByTagName('li');

		var oldLIsLength = oldLIs.length;
		var newLIsLength = newLIs.length;

		for (var i=0, n=0; i < newLIs.length; i++, n++)
		{
			var newLI = newLIs[i];

			if (n < oldLIsLength)
			{
				var oldLI = oldLIs[n];

				var oldHtml = oldLI.outerHTML;
				var newHtml = newLI.outerHTML;
				if (oldHtml !== newHtml)
				{
					oldPager.replaceChild(newLI, oldLI);
					i--;
				}
			}
			else
			{
				oldPager.appendChild(newLI);
				i--;
			}
		}

		while (oldLIs.length > newLIsLength)
		{
			oldPager.removeChild(oldPager.lastChild);
		}
	}

	function updateInfo(data)
	{
		var infoText;
		var firstRecord, lastRecord;

		if (data.content.length === 0)
		{
			infoText = data.config.infoEmpty;
		}
		else if (data.curPage === 0)
		{
			infoText = 'No matching records found (total ' + data.content.length + ')';
		}
		else
		{
			firstRecord = (data.curPage - 1) * data.pageSize + 1;
			lastRecord = firstRecord + data.pageContent.length - 1;
			infoText = 'Showing records ' + firstRecord + '-' + lastRecord + ' from ' + data.filteredContent.length;
			if (data.filteredContent.length != data.content.length)
			{
				infoText += ' filtered (total ' + data.content.length + ')';
			}
		}
		data.config.infoContainer.html(infoText);

		if (data.config.updateInfoCallback)
		{
			data.config.updateInfoCallback({
				total: data.content.length,
				available: data.availableContent.length,
				filtered: data.filteredContent.length,
				firstRecord: firstRecord,
				lastRecord: lastRecord
			});
		}
	}

	function updateSelector(data)
	{
		data.pageCheckedCount = 0;
		if (data.checkedCount > 0 && data.filteredContent.length > 0)
		{
			for (var i = (data.curPage - 1) * data.pageSize; i < Math.min(data.curPage * data.pageSize, data.filteredContent.length); i++)
			{
				data.pageCheckedCount += data.checkedRows[data.filteredContent[i].id] ? 1 : 0;
			}
		}

		var selectorEl = data.config.selector[0];
		if (selectorEl)
		{
			selectorEl.style.display = data.pageCheckedCount === data.checkedCount ? 'none' : '';
			if (data.checkedCount !== data.pageCheckedCount)
			{
				var diff = data.checkedCount - data.pageCheckedCount;
				selectorEl.textContent = diff + (diff > 1 ? ' records' : ' record') + ' selected on other pages';
			}
		}
	}

	function setPageSize(pageSize, maxPages, pageDots)
	{
		var data = $(this).data('fasttable');
		data.pageSize = parseInt(pageSize);
		data.curPage = 1;
		if (maxPages !== undefined)
		{
			data.maxPages = maxPages;
		}
		if (pageDots !== undefined)
		{
			data.pageDots = pageDots;
		}
		refresh(data);
	}

	function setCurPage(page)
	{
		var data = $(this).data('fasttable');
		data.curPage = parseInt(page);
		refresh(data);
	}

	function checkedIds(data)
	{
		var checkedRows = data.checkedRows;
		var checkedIds = [];
		for (var i = 0; i < data.content.length; i++)
		{
			var id = data.content[i].id;
			if (checkedRows[id])
			{
				checkedIds.push(id);
			}
		}
		return checkedIds;
	}

	function titleCheckRedraw(data)
	{
		var filteredContent = data.filteredContent;
		var checkedRows = data.checkedRows;

		var hasSelectedItems = false;
		var hasUnselectedItems = false;
		for (var i = 0; i < filteredContent.length; i++)
		{
			if (checkedRows[filteredContent[i].id])
			{
				hasSelectedItems = true;
			}
			else
			{
				hasUnselectedItems = true;
			}
		}

		var headerRow = data.tableEl.tHead ? data.tableEl.tHead.rows[0] : null;
		if (!headerRow) return;

		if (hasSelectedItems && hasUnselectedItems)
		{
			headerRow.classList.remove('checked');
			headerRow.classList.add('checkremove');
		}
		else if (hasSelectedItems)
		{
			headerRow.classList.remove('checkremove');
			headerRow.classList.add('checked');
		}
		else
		{
			headerRow.classList.remove('checked', 'checkremove');
		}
	}

	function resolveTarget(event)
	{
		var target = event.target;
		if (!(target instanceof Element)) target = target.parentElement;
		return target;
	}

	function itemCheckClick(data, event)
	{
		var target = resolveTarget(event);
		if (!target) return;
		var checkmark = target.classList.contains('check');
		if (data.dragging || (!checkmark && !data.config.rowSelect))
		{
			return;
		}

		var row = target.closest('tr');
		if (!row) return;
		var id = row.fasttableID;
		var doToggle = true;
		var checkedRows = data.checkedRows;

		if (event.shiftKey && data.lastClickedRowID != null)
		{
			var checked = checkedRows[id];
			doToggle = !checkRange(data, id, data.lastClickedRowID, !checked);
		}

		if (doToggle)
		{
			toggleCheck(data, id);
		}

		data.lastClickedRowID = id;

		refresh(data);
	}

	function titleCheckClick(data, event)
	{
		var target = resolveTarget(event);
		if (!target) return;
		var checkmark = target.classList.contains('check');
		if (data.dragging || (!checkmark && !data.config.rowSelect))
		{
			return;
		}

		var filteredContent = data.filteredContent;
		var checkedRows = data.checkedRows;

		var hasSelectedItems = false;
		for (var i = 0; i < filteredContent.length; i++)
		{
			if (checkedRows[filteredContent[i].id])
			{
				hasSelectedItems = true;
				break;
			}
		}

		data.lastClickedRowID = null;
		checkAll(data, !hasSelectedItems);
	}

	function toggleCheck(data, id)
	{
		var checkedRows = data.checkedRows;
		if (checkedRows[id])
		{
			checkedRows[id] = undefined;
			data.checkedCount--;
		}
		else
		{
			checkedRows[id] = true;
			data.checkedCount++;
		}
	}

	function checkAll(data, checked)
	{
		var filteredContent = data.filteredContent;

		for (var i = 0; i < filteredContent.length; i++)
		{
			checkRow(data, filteredContent[i].id, checked);
		}

		refresh(data);
	}

	function checkRange(data, from, to, checked)
	{
		var filteredContent = data.filteredContent;
		var indexFrom = indexOfID(filteredContent, from);
		var indexTo = indexOfID(filteredContent, to);
		if (indexFrom === -1 || indexTo === -1)
		{
			return false;
		}

		if (indexTo < indexFrom)
		{
			var tmp = indexTo; indexTo = indexFrom; indexFrom = tmp;
		}

		for (var i = indexFrom; i <= indexTo; i++)
		{
			checkRow(data, filteredContent[i].id, checked);
		}

		return true;
	}

	function checkRow(data, id, checked)
	{
		if (checked)
		{
			if (!data.checkedRows[id])
			{
				data.checkedCount++;
			}
			data.checkedRows[id] = true;
		}
		else
		{
			if (data.checkedRows[id])
			{
				data.checkedCount--;
			}
			data.checkedRows[id] = undefined;
		}
	}

	function indexOfID(content, id)
	{
		for (var i = 0; i < content.length; i++)
		{
			if (id === content[i].id)
			{
				return i;
			}
		}
		return -1;
	}

	function validateChecks(data)
	{
		var checkedRows = data.checkedRows;
		data.checkedRows = {}
		data.checkedCount = 0;
		for (var i = 0; i < data.content.length; i++)
		{
			if (checkedRows[data.content[i].id])
			{
				data.checkedRows[data.content[i].id] = true;
				data.checkedCount++;
			}
		}
	}

	//*************** DRAG-N-DROP (Pointer Events)

	function initDragDrop(data)
	{
		var el = data.tableEl;
		data.onPointerDown = function(e) { pointerDown(data, e); };
		el.addEventListener('pointerdown', data.onPointerDown);

		data.moveIds = [];
		data.dropAfter = false;
		data.dropId = null;
		data.dragging = false;
		data.dragRow = $('');
		data.cancelDrag = false;
		data.downPos = null;
		data.blinkIds = [];
		data.blinkState = null;
		data.wantBlink = false;
		data.blinkTimer = null;
		data.activePointerId = null;
	}

	function pointerDown(data, e)
	{
		if (data.isInteracting) return;

		data.dragging = false;
		data.dropId = null;

		var target = resolveTarget(e);
		if (!target) return;
		var row = target.closest('tr');
		if (!row || !data.tableEl.contains(row)) return;
		data.dragRow = $(row);

		var checkmark = target.classList.contains('check') ||
			(target.querySelector('.check') && !document.body.classList.contains('phone'));
		var head = row.parentNode && row.parentNode.tagName === 'THEAD';
		if (head || !(checkmark || (data.config.rowSelect && e.pointerType === 'mouse')) ||
			e.ctrlKey || e.altKey || e.metaKey)
		{
			return;
		}

		if (e.pointerType === 'mouse')
		{
			e.preventDefault();
		}

		if (!data.config.dragEndCallback)
		{
			return;
		}

		data.downPos = { x: e.clientX, y: e.clientY };
		data.isInteracting = true;
		data.activePointerId = e.pointerId;

		data.onPointerMove = function(ev) { pointerMove(data, ev); };
		data.onPointerUp = function(ev) { pointerUp(data, ev); };
		data.onKeyDown = function(ev) { dragKeyDown(data, ev); };

		document.addEventListener('pointermove', data.onPointerMove, true);
		document.addEventListener('pointerup', data.onPointerUp, true);
		document.addEventListener('pointercancel', data.onPointerUp, true);
		document.addEventListener('keydown', data.onKeyDown, true);
	}

	function pointerMove(data, e)
	{
		if (e.pointerId !== data.activePointerId) return;

		e.preventDefault();

		if (!data.dragging)
		{
			if (Math.abs(data.downPos.x - e.clientX) < 5 &&
				Math.abs(data.downPos.y - e.clientY) < 5)
			{
				return;
			}
			startDrag(data, e);
			if (data.cancelDrag)
			{
				pointerUp(data, e);
				return;
			}
		}

		updateDrag(data, e.clientX, e.clientY);
		autoScroll(data, e.clientX, e.clientY);
	}

	function startDrag(data, e)
	{
		if (data.config.dragStartCallback)
		{
			data.config.dragStartCallback();
		}

		var offsetX = window.scrollX;
		var offsetY = window.scrollY;
		var rf = data.dragRow[0].getBoundingClientRect();
		data.dragOffset = {
			x: data.downPos.x - rf.left,
			y: Math.min(Math.max(data.downPos.y - rf.top, 0), rf.height)
		};

		var checkedRows = data.checkedRows;
		var chkIds = checkedIds(data);
		var id = data.dragRow[0].fasttableID;
		data.moveIds = checkedRows[id] ? chkIds : [id];
		data.dragging = true;
		data.cancelDrag = false;

		try {
			data.dragRow[0].setPointerCapture(data.activePointerId);
		} catch (ex) {}

		buildDragBox(data);
		data.config.dragBox[0].style.display = 'block';
		data.dragRow[0].classList.add('drag-source');
		document.documentElement.classList.add('drag-progress');
		data.oldOverflowX = document.body.style.overflowX;
		document.body.style.overflowX = 'hidden';
	}

	function buildDragBox(data)
	{
		var tr = data.dragRow.clone();
		var table = data.target.clone();
		$('tr', table).remove();
		$('thead', table).remove();
		$('tbody', table).append(tr);
		table.css('margin', 0);
		data.config.dragContent.html(table);
		data.config.dragBadge.text(data.moveIds.length);
		data.config.dragBadge.css('display', data.moveIds.length > 1 ? 'block' : 'none');

		var dragRowRect = data.dragRow[0].getBoundingClientRect();
		data.config.dragBox.css({left: dragRowRect.left + window.scrollX, width: dragRowRect.width});

		var tds = $('td', tr);
		var srcTds = data.dragRow[0].cells;
		for (var i = 0; i < srcTds.length && i < tds.length; i++)
		{
			tds[i].style.width = srcTds[i].offsetWidth + 'px';
		}
	}

	function updateDrag(data, x, y)
	{
		var offsetX = window.scrollX;
		var offsetY = window.scrollY;
		var posX = x + offsetX;
		var posY = y + offsetY;

		var dragBoxEl = data.config.dragBox[0];
		var dragBoxHeight = dragBoxEl.offsetHeight;
		var winHeight = window.innerHeight;

		dragBoxEl.style.left = (posX - data.dragOffset.x) + 'px';
		dragBoxEl.style.top = Math.max(Math.min(posY - data.dragOffset.y, offsetY + winHeight - dragBoxHeight - 2), offsetY + 2) + 'px';

		var dragBoxRect = dragBoxEl.getBoundingClientRect();
		var dt = dragBoxRect.top + offsetY;
		var dh = dragBoxRect.height;

		var tbody = data.tableEl.tBodies[0];
		if (!tbody) return;
		var rows = tbody.rows;

		for (var i = 0; i < rows.length; i++)
		{
			var rowEl = rows[i];
			if (rowEl === data.dragRow[0]) continue;

			var rowRect = rowEl.getBoundingClientRect();
			var rt = rowRect.top + offsetY;
			var rh = rowRect.height;

			if ((dt >= rt && dt <= rt + rh / 2) ||
				(dt < rt && i === 0))
			{
				data.dropAfter = false;
				rowEl.parentNode.insertBefore(data.dragRow[0], rowEl);
				data.dropId = rowEl.fasttableID;
				break;
			}
			if ((dt + dh >= rt + rh / 2 && dt + dh <= rt + rh) ||
				(dt + dh > rt + rh && i === rows.length - 1))
			{
				data.dropAfter = true;
				rowEl.parentNode.insertBefore(data.dragRow[0], rowEl.nextSibling);
				data.dropId = rowEl.fasttableID;
				break;
			}
		}

		if (data.dropId === null)
		{
			data.dropId = data.dragRow[0].fasttableID;
			data.dropAfter = true;
		}
	}

	function autoScroll(data, x, y)
	{
		var winHeight = window.innerHeight;
		data.scrollStep = (y > winHeight - 20 ? 1 : y < 20 ? -1 : 0) * 5;
		if (data.scrollStep !== 0 && !data.scrollTimer)
		{
			var scroll = function()
			{
				window.scrollBy(0, data.scrollStep);
				updateDrag(data, x, y + data.scrollStep);
				data.scrollTimer = data.scrollStep == 0 ? null : setTimeout(scroll, 10);
			}
			data.scrollTimer = setTimeout(scroll, 500);
		}
	}

	function pointerUp(data, e)
	{
		var pid = (e && e.pointerId !== undefined) ? e.pointerId : data.activePointerId;
		if (pid !== data.activePointerId) return;

		document.removeEventListener('pointermove', data.onPointerMove, true);
		document.removeEventListener('pointerup', data.onPointerUp, true);
		document.removeEventListener('pointercancel', data.onPointerUp, true);
		document.removeEventListener('keydown', data.onKeyDown, true);

		data.isInteracting = false;
		data.activePointerId = null;

		if (!data.dragging)
		{
			return;
		}

		data.dragging = false;
		data.cancelDrag = data.cancelDrag || (e && e.type === 'pointercancel');

		try {
			data.dragRow[0].releasePointerCapture(pid);
		} catch (ex) {}

		data.dragRow[0].classList.remove('drag-source');
		document.documentElement.classList.remove('drag-progress');
		document.body.style.overflowX = data.oldOverflowX;
		data.config.dragBox[0].style.display = 'none';
		data.scrollStep = 0;
		clearTimeout(data.scrollTimer);
		data.scrollTimer = null;
		moveRecords(data);

		scheduleRender(data, false);
	}

	function dragKeyDown(data, e)
	{
		if (e.key === 'Escape')
		{
			data.cancelDrag = true;
			e.preventDefault();
			pointerUp(data, null);
		}
	}

	function moveRecords(data)
	{
		if (data.dropId !== null && !data.cancelDrag &&
			!(data.moveIds.length == 1 && data.dropId == data.moveIds[0]))
		{
			data.blinkIds = data.moveIds;
			data.moveIds = [];
			data.blinkState = data.config.dragBlink === 'none' ? 0 : 3;
			data.wantBlink = data.blinkState > 0;
			moveRows(data);
		}
		else
		{
			data.dropId = null;
		}

		if (data.dropId === null)
		{
			data.moveIds = [];
		}

		refresh(data);

		data.config.dragEndCallback(data.dropId !== null ?
			{
				ids: data.blinkIds,
				position: data.dropId,
				direction: data.dropAfter ? 'after' : 'before'
			} : null);

		if (data.config.dragBlink === 'direct')
		{
			data.target.fasttable('update');
		}
	}

	function moveRows(data)
	{
		var movedIds = data.blinkIds;
		var movedRecords = [];

		for (var i = 0; i < data.content.length; i++)
		{
			var item = data.content[i];
			if (movedIds.indexOf(item.id) > -1)
			{
				movedRecords.push(item);
				data.content.splice(i, 1);
				i--;

				if (item.id === data.dropId)
				{
					if (i >= 0)
					{
						data.dropId = data.content[i].id;
						data.dropAfter = true;
					}
					else if (i + 1 < data.content.length)
					{
						data.dropId = data.content[i + 1].id;
						data.dropAfter = false;
					}
					else
					{
						data.dropId = null;
					}
				}
			}
		}

		if (data.dropId === null)
		{
			for (var j = 0; j < movedRecords.length; j++)
			{
				data.content.push(movedRecords[j]);
			}
			return;
		}

		for (var i = 0; i < data.content.length; i++)
		{
			if (data.content[i].id === data.dropId)
			{
				for (var j = movedRecords.length - 1; j >= 0; j--)
				{
					data.content.splice(data.dropAfter ? i + 1 : i, 0, movedRecords[j]);
				}
				break;
			}
		}
	}

	function blinkMovedRecords(data)
	{
		if (data.blinkIds.length === 0) return;
		if (data.blinkTimer) return;
		blinkProgress(data, data.wantBlink);
		data.wantBlink = false;
	}

	function blinkProgress(data, recur)
	{
		var tbody = data.tableEl.tBodies[0];
		if (!tbody) return;

		var rows = tbody.rows;
		for (var i = 0; i < rows.length; i++)
		{
			var el = rows[i];
			var id = el.fasttableID;
			if (data.blinkIds.indexOf(id) > -1 &&
				(data.blinkState === 1 || data.blinkState === 3 || data.blinkState === 5))
			{
				el.classList.add('drag-finish');
			}
			else
			{
				el.classList.remove('drag-finish');
			}
		}

		if (recur && data.blinkState > 0)
		{
			data.blinkTimer = setTimeout(function()
				{
					data.blinkState -= 1;
					data.blinkTimer = null;
					blinkProgress(data, true);
				},
				150);
		}

		if (data.blinkState === 0)
		{
			data.blinkIds = [];
		}
	}

	//*************** KEYBOARD

	function processShortcut(data, key)
	{
		switch (key)
		{
			case 'Left': data.curPage = Math.max(data.curPage - 1, 1); refresh(data); return true;
			case 'Shift+Left': data.curPage = 1; refresh(data); return true;
			case 'Right': data.curPage = Math.min(data.curPage + 1, data.pageCount); refresh(data); return true;
			case 'Shift+Right': data.curPage = data.pageCount; refresh(data); return true;
			case 'Shift+F': data.config.filterInput.focus(); return true;
			case 'Shift+C': data.config.filterClearButton.click(); return true;
		}
	}

	//*************** CONFIG

	var defaults =
	{
		filterInput: '#TableFilter',
		filterClearButton: '#TableClear',
		pagerContainer: '#TablePager',
		infoContainer: '#TableInfo',
		dragBox: '#TableDragBox',
		dragContent: '#TableDragContent',
		dragBadge: '#TableDragBadge',
		dragBlink: 'none', // none, direct, update
		pageSize: 10,
		maxPages: 5,
		pageDots: true,
		rowSelect: false,
		shortcuts: false,
		infoEmpty: 'No records',
		renderRowCallback: undefined,
		renderCellCallback: undefined,
		renderTableCallback: undefined,
		fillFieldsCallback: undefined,
		updateInfoCallback: undefined,
		filterInputCallback: undefined,
		filterClearCallback: undefined,
		fillSearchCallback: undefined,
		filterCallback: undefined,
		dragStartCallback: undefined,
		dragEndCallback: undefined
	};

})(jQuery);

function FastSearcher()
{
	'use strict';

	this.source;
	this.len = 0;
	this.p = 0;

	this.initLexer = function(source)
	{
		this.source = source;
		this.len = source.length;
		this.p = 0;
	}

	this.nextToken = function()
	{
		while (this.p < this.len)
		{
			var ch = this.source[this.p++];
			switch (ch) {
				case ' ':
				case '\t':
					continue;

				case '-':
				case '(':
				case ')':
				case '|':
					return ch;

				default:
					this.p--;
					var token = '';
					var quote = false;
					while (this.p < this.len)
					{
						var ch = this.source[this.p++];
						if (quote)
						{
							if (ch === '"')
							{
								quote = false;
								ch = '';
							}
						}
						else
						{
							if (ch === '"')
							{
								quote = true;
								ch = '';
							}
							else if (' \t()|'.indexOf(ch) > -1)
							{
								this.p--;
								return token;
							}
						}
						token += ch;
					}
					return token;
			}
		}
		return null;
	}

	this.compile = function(searchstr)
	{
		var _this = this;
		this.initLexer(searchstr);

		function expression(greedy)
		{
			var node = null;
			while (true)
			{
				var token = _this.nextToken();
				var node2 = null;
				switch (token)
				{
					case null:
					case ')':
						return node;

					case '-':
						node2 = expression(false);
						node2 = node2 ? _this.not(node2) : node2;
						break;

					case '(':
						node2 = expression(true);
						break;

					case '|':
						node2 = expression(false);
						break;

					default:
						node2 = _this.term(token);
				}

				if (node && node2)
				{
					node = token === '|' ? _this.or(node, node2) : _this.and(node, node2);
				}
				else if (node2)
				{
					node = node2;
				}

				if (!greedy && node)
				{
					return node;
				}
			}
		}

		this.root = expression(true);
	}

	this.root = null;
	this.data = null;

	this.exec = function(data) {
		this.data = data;
		return this.root ? this.root.eval() : true;
	}

	this.and = function(L, R) {
		return {
			L: L, R: R,
			eval: function() { return this.L.eval() && this.R.eval(); }
		};
	}

	this.or = function(L, R) {
		return {
			L: L, R: R,
			eval: function() { return this.L.eval() || this.R.eval(); }
		};
	}

	this.not = function(M) {
		return {
			M: M,
			eval: function() { return !this.M.eval();}
		};
	}

	this.term = function(term) {
		return this.compileTerm(term);
	}

	var COMMANDS = [ ':', '>=', '<=', '<>', '>', '<', '=' ];

	this.compileTerm = function(term) {
		var _this = this;
		var text = term.toLowerCase();
		var field;

		var command;
		var commandIndex;
		for (var i = 0; i < COMMANDS.length; i++)
		{
			var cmd = COMMANDS[i];
			var p = term.indexOf(cmd);
			if (p > -1 && (p < commandIndex || commandIndex === undefined))
			{
				commandIndex = p;
				command = cmd;
			}
		}

		if (command !== undefined)
		{
			field = term.substring(0, commandIndex);
			text = text.substring(commandIndex + command.length);
		}

		return {
			command: command,
			text: text,
			field: field,
			eval: function() { return _this.evalTerm(this); }
		};
	}

	this.evalTerm = function(term) {
		var text = term.text;
		var field = term.field;
		var content = this.fieldValue(this.data, field);

		if (content === undefined)
		{
			return false;
		}

		switch (term.command)
		{
			case undefined:
			case ':':
				return content.toString().toLowerCase().indexOf(text) > -1;
			case '=':
				return content.toString().toLowerCase() == text;
			case '<>':
				return content.toString().toLowerCase() != text;
			case '>':
				return parseInt(content) > parseInt(text);
			case '>=':
				return parseInt(content) >= parseInt(text);
			case '<':
				return parseInt(content) < parseInt(text);
			case '<=':
				return parseInt(content) <= parseInt(text);
			default:
				return false;
		}
	}

	this.fieldValue = function(data, field) {
		var value = '';
		if (field !== undefined)
		{
			value = data[field];
			if (value === undefined)
			{
				if (this.nameMap === undefined)
				{
					this.buildNameMap(data);
				}
				value = data[this.nameMap[field.toLowerCase()]];
			}
		}
		else
		{
			if (data._search === true)
			{
				for (var prop in data)
				{
					value += ' ' + data[prop];
				}
			}
			else
			{
				for (var i = 0; i < data._search.length; i++)
				{
					value += ' ' + data[data._search[i]];
				}
			}
		}
		return value;
	}

	this.nameMap = undefined;
	this.buildNameMap = function(data)
	{
		this.nameMap = {};
		for (var prop in data)
		{
			this.nameMap[prop.toLowerCase()] = prop;
		}
	}
}
