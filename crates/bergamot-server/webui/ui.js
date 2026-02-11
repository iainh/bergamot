/*
 *
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

(function($) {
	'use strict';

	// =========================================================================
	// Modal
	// =========================================================================

	function Modal($el, options) {
		this.$el = $el;
		this.options = $.extend({}, Modal.DEFAULTS, options);
		this.$backdrop = null;
		this.isShown = false;

		var self = this;
		$el.on('click', '[data-dismiss="modal"]', function(e) {
			e.preventDefault();
			self.hide();
		});
	}

	Modal.DEFAULTS = {
		backdrop: true
	};

	Modal.prototype.show = function() {
		if (this.isShown) return;

		var self = this;
		var e = $.Event('show');
		this.$el.trigger(e);
		if (e.isDefaultPrevented()) return;

		this.isShown = true;
		this.$el.removeClass('hide').addClass('in').show();
		$('body').addClass('modal-open');

		if (this.options.backdrop !== false) {
			this.$backdrop = $('<div class="modal-backdrop fade in"></div>');
			$('body').append(this.$backdrop);

			if (this.options.backdrop !== 'static') {
				this.$backdrop.on('click', function() {
					self.hide();
				});
			}
		}

		this.$el.trigger('shown');
	};

	Modal.prototype.hide = function() {
		if (!this.isShown) return;

		var e = $.Event('hide');
		this.$el.trigger(e);
		if (e.isDefaultPrevented()) return;

		this.isShown = false;
		this.$el.addClass('hide').removeClass('in').hide();
		$('body').removeClass('modal-open');

		if (this.$backdrop) {
			this.$backdrop.remove();
			this.$backdrop = null;
		}

		this.$el.trigger('hidden');
	};

	$.fn.modal = function(option) {
		return this.each(function() {
			var $this = $(this);
			var data = $this.data('ui.modal');
			var options = typeof option === 'object' && option;

			if (!data) {
				data = new Modal($this, options);
				$this.data('ui.modal', data);
			} else if (typeof option === 'object') {
				data.options = $.extend({}, Modal.DEFAULTS, option);
			}

			if (typeof option === 'string') {
				data[option]();
			} else {
				data.show();
			}
		});
	};

	$(document).on('click', '[data-toggle="modal"]', function(e) {
		e.preventDefault();
		var target = $(this).attr('href') || $(this).attr('data-target');
		if (target) {
			$(target).modal();
		}
	});

	// =========================================================================
	// Dropdown
	// =========================================================================

	$(document).on('click', '[data-toggle="dropdown"]', function(e) {
		e.preventDefault();
		e.stopPropagation();
		var $parent = $(this).closest('.btn-group, .dropdown');
		$('.btn-group.open, .dropdown.open').not($parent).removeClass('open');
		$parent.toggleClass('open');
	});

	$(document).on('click', function() {
		$('.btn-group.open, .dropdown.open').removeClass('open');
	});

	// =========================================================================
	// Tab
	// =========================================================================

	function Tab($el) {
		this.$el = $el;
	}

	Tab.prototype.show = function() {
		var $this = this.$el;
		var $li = $this.closest('li');

		if ($li.hasClass('active')) return;

		var e = $.Event('show');
		$this.trigger(e);
		if (e.isDefaultPrevented()) return;

		var selector = $this.attr('href');
		var $target = $(selector);

		$li.siblings().removeClass('active');
		$li.addClass('active');

		var $pane = $target.closest('.tab-content');
		$pane.children('.tab-pane').removeClass('active in');
		$target.addClass('active in');

		$this.trigger('shown');
	};

	$.fn.tab = function(action) {
		return this.each(function() {
			var $this = $(this);
			var data = $this.data('ui.tab');
			if (!data) {
				data = new Tab($this);
				$this.data('ui.tab', data);
			}
			if (typeof action === 'string') {
				data[action]();
			}
		});
	};

	$(document).on('click', '[data-toggle="tab"]', function(e) {
		e.preventDefault();
		$(this).tab('show');
	});

	// =========================================================================
	// Collapse
	// =========================================================================

	$(document).on('click', '[data-toggle="collapse"]', function(e) {
		e.preventDefault();
		var target = $(this).attr('data-target') || $(this).attr('href');
		if (!target) return;

		var $target = $(target);
		var $parent = $(this).attr('data-parent');

		if ($parent) {
			$($parent).find('.collapse.in').not($target).removeClass('in');
		}

		$target.toggleClass('in');
	});

})(jQuery);
