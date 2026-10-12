// Schema-driven widget template editor.
//
// Reads:    window.WIDGET_SCHEMAS  (from widget-schemas.js)
// Exports:  window.WidgetTemplateEditor with methods:
//           renderForm(container, schema, currentProps, onChange)
//           collectProps(container, schema) → object
//
// The form is built fresh from the schema each time the widget type changes.
// A "Custom Properties" section lets users add arbitrary key/value rows for
// advanced/experimental props.

(function () {
  // HTML-escape for safe interpolation
  function esc(s) {
    return String(s ?? '').replace(/[&<>"']/g, c => ({
      '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;',
    }[c]));
  }

  // Render a single field's input element
  function renderField(field, currentProps) {
    const value = currentProps[field.key] !== undefined
      ? currentProps[field.key]
      : (field.default !== undefined ? field.default : '');
    const id = `wtfk-${field.key}`;
    let input = '';

    switch (field.type) {
      case 'string':
        // `browse: 'fonts'` adds a font-picker button next to the
        // input. The picker is the same iframe used by the layout
        // editor; the value flows back through a postMessage handler
        // installed below.
        const fontBrowse = field.browse === 'fonts'
          ? `<button type="button" class="btn btn-secondary btn-sm" data-wtfkbrowse-font="${esc(field.key)}" title="Browse fonts">🅰</button>`
          : '';
        input = `<div class="wtfk-url-row">
                   <input type="text" id="${id}" data-wtfk="${esc(field.key)}" data-wtftype="string"
                    value="${esc(value)}" ${field.placeholder ? `placeholder="${esc(field.placeholder)}"` : ''}
                    class="text-input" style="flex:1;">
                   ${fontBrowse}
                 </div>`;
        break;

      case 'text':
        input = `<textarea id="${id}" data-wtfk="${esc(field.key)}" data-wtftype="text" rows="3"
                 class="text-input"
                 ${field.placeholder ? `placeholder="${esc(field.placeholder)}"` : ''}>${esc(value)}</textarea>`;
        break;

      case 'number':
        input = `<input type="number" id="${id}" data-wtfk="${esc(field.key)}" data-wtftype="number"
                 value="${esc(value)}" ${field.min !== undefined ? `min="${field.min}"` : ''}
                 ${field.max !== undefined ? `max="${field.max}"` : ''}
                 ${field.step !== undefined ? `step="${field.step}"` : ''}
                 class="text-input">`;
        break;

      case 'range':
        input = `<div style="display:flex; align-items:center; gap:0.5rem;">
                   <input type="range" id="${id}" data-wtfk="${esc(field.key)}" data-wtftype="range"
                    value="${esc(value)}" ${field.min !== undefined ? `min="${field.min}"` : ''}
                    ${field.max !== undefined ? `max="${field.max}"` : ''}
                    ${field.step !== undefined ? `step="${field.step}"` : ''}
                    style="flex:1;">
                   <span id="${id}-val" class="wtfk-range-val">${esc(value)}</span>
                 </div>`;
        break;

      case 'boolean':
        input = `<label class="wtfk-toggle">
                   <input type="checkbox" id="${id}" data-wtfk="${esc(field.key)}" data-wtftype="boolean"
                    ${value ? 'checked' : ''}>
                   <span class="wtfk-toggle-slider"></span>
                   <span class="wtfk-toggle-label">${value ? 'On' : 'Off'}</span>
                 </label>`;
        break;

      case 'color':
        input = `<input type="color" id="${id}" data-wtfk="${esc(field.key)}" data-wtftype="color"
                 value="${esc(value || '#000000')}" class="wtfk-color-input">`;
        break;

      case 'url':
      case 'file':
        input = `<div class="wtfk-url-row">
                   <input type="url" id="${id}" data-wtfk="${esc(field.key)}" data-wtftype="url"
                    value="${esc(value)}" ${field.placeholder ? `placeholder="${esc(field.placeholder)}"` : ''}
                    class="text-input" style="flex:1;">
                   ${field.browse === 'file' ? `<button type="button" class="btn btn-secondary btn-sm" data-wtfkbrowse="${esc(field.key)}" title="Browse uploaded files">📁</button>` : ''}
                 </div>`;
        break;

      case 'select':
        input = `<select id="${id}" data-wtfk="${esc(field.key)}" data-wtftype="select" class="select">
                   ${field.options.map(o => `
                     <option value="${esc(o.value)}"
                       ${String(value) === String(o.value) ? 'selected' : ''}>
                       ${esc(o.label)}
                     </option>`).join('')}
                 </select>`;
        break;

      case 'radio':
        input = `<div class="wtfk-radio-group">
                   ${field.options.map(o => `
                     <label class="wtfk-radio ${String(value) === String(o.value) ? 'active' : ''}">
                       <input type="radio" data-wtfk="${esc(field.key)}" data-wtftype="radio"
                        name="wtfk-${esc(field.key)}" value="${esc(o.value)}"
                        ${String(value) === String(o.value) ? 'checked' : ''}>
                       <span>${esc(o.label)}</span>
                     </label>`).join('')}
                 </div>`;
        break;

      default:
        input = `<input type="text" id="${id}" data-wtfk="${esc(field.key)}" data-wtftype="string"
                 value="${esc(value)}" class="text-input">`;
    }

    const hint = field.hint ? `<span class="wtfk-hint">${esc(field.hint)}</span>` : '';
    const help = field.help ? `<div class="wtfk-help">${esc(field.help)}</div>` : '';
    const required = field.required ? `<span class="wtfk-required" title="Required">*</span>` : '';

    return `<div class="wtfk-field" data-wtfk-section="${esc(field.section)}" data-wtfk-name="${esc(field.key)}">
              <label class="wtfk-label" for="${id}">
                <span>${esc(field.label)}${required}</span>
                ${hint}
              </label>
              <div class="wtfk-input-wrap">${input}</div>
              ${help}
            </div>`;
  }

  // Render the form into a container
  function renderForm(container, schema, currentProps, onChange) {
    const props = currentProps || {};
    // Group fields by section
    const sections = (schema.sections || []).map(s => ({
      ...s,
      fields: (schema.fields || []).filter(f => f.section === s.id),
    })).filter(s => s.fields.length > 0);

    const html = sections.map((s, idx) => {
      const open = idx === 0 ? 'open' : '';
      const visibleFields = s.fields.filter(f => window.fieldIsVisible(f, props));
      if (visibleFields.length === 0) return '';
      return `<details class="tpl-prop-section" ${open}>
                <summary><span>${esc(s.icon)} ${esc(s.label)}</span><small>${esc(visibleFields.length)} field${visibleFields.length === 1 ? '' : 's'}</small></summary>
                <div class="tpl-prop-section-body">
                  ${visibleFields.map(f => renderField(f, props)).join('')}
                </div>
              </details>`;
    }).join('');

    container.innerHTML = `<div class="wtfk-form">${html}</div>`;

    // If showIf dependencies changed, re-render to add/remove fields
    container._wtfProps = props;

    // Wire up listeners
    container.querySelectorAll('[data-wtfk]').forEach(el => {
      const key = el.dataset.wtfk;
      const type = el.dataset.wtftype;
      const event = (type === 'select' || type === 'boolean' || type === 'radio') ? 'change' : 'input';
      el.addEventListener(event, (e) => {
        let val;
        if (type === 'boolean') {
          val = el.checked;
        } else if (type === 'number' || type === 'range') {
          val = el.value === '' ? '' : Number(el.value);
        } else if (type === 'radio') {
          val = el.value;
        } else {
          val = el.value;
        }
        props[key] = val;
        if (type === 'range') {
          const lbl = container.querySelector(`#${el.id}-val`);
          if (lbl) lbl.textContent = val;
        }
        if (type === 'boolean') {
          const lbl = el.closest('.wtfk-toggle')?.querySelector('.wtfk-toggle-label');
          if (lbl) lbl.textContent = val ? 'On' : 'Off';
        }
        if (type === 'radio') {
          el.closest('.wtfk-radio-group').querySelectorAll('.wtfk-radio').forEach(r => {
            r.classList.toggle('active', r.querySelector('input').value === val);
          });
        }
        // If any field has showIf dependencies on this key, re-render
        if (hasShowIfDeps(schema, key)) {
          renderForm(container, schema, props, onChange);
        }
        if (onChange) onChange(key, val, props);
      });
    });

    // Wire up browse buttons
    container.querySelectorAll('[data-wtfkbrowse]').forEach(btn => {
      btn.addEventListener('click', () => {
        const key = btn.dataset.wtfkbrowse;
        if (window.App && window.App.openFilePicker) {
          window.App.openFilePicker((picked) => {
            const input = container.querySelector(`[data-wtfk="${key}"]`);
            if (input) {
              input.value = picked.url;
              input.dispatchEvent(new Event('input', { bubbles: true }));
            }
          });
        } else {
          alert('File picker not available.');
        }
      });
    });

    // Wire up font-picker buttons. Same postMessage contract as
    // the layout editor: the iframe posts { type: 'font-picked',
    // cssValue } and we drop the value into the target input +
    // fire an 'input' event so the existing onChange fires.
    container.querySelectorAll('[data-wtfkbrowse-font]').forEach(btn => {
      btn.addEventListener('click', () => {
        const key = btn.dataset.wtfkbrowseFont;
        const target = container.querySelector(`[data-wtfk="${key}"]`);
        const current = target ? target.value : '';
        // Use a global registry so multiple picker overlays don't
        // collide. Each call gets a unique id; the message handler
        // checks for it before forwarding.
        const pickerId = 'wtfk-font-' + Math.random().toString(36).slice(2);
        const overlay = document.createElement('div');
        overlay.style.cssText = 'position:fixed;inset:0;background:rgba(0,0,0,0.7);z-index:9999;display:flex;align-items:center;justify-content:center;';
        const box = document.createElement('div');
        box.style.cssText = 'width:min(900px,95vw);height:min(700px,90vh);background:var(--bg,#0d1117);border:1px solid var(--border,#30363d);border-radius:8px;overflow:hidden;display:flex;flex-direction:column;';
        const header = document.createElement('div');
        header.style.cssText = 'padding:10px 16px;border-bottom:1px solid var(--border,#30363d);display:flex;align-items:center;justify-content:space-between;background:var(--bg-card,#161b22);';
        header.innerHTML = '<span style="font-weight:600;color:var(--text,#e6edf3);">Font Picker</span><button type="button" id="wtfk-font-close" style="padding:4px 10px;border-radius:4px;border:1px solid var(--border,#30363d);background:var(--bg-elev,#21262d);color:var(--text,#e6edf3);cursor:pointer;">×</button>';
        const iframe = document.createElement('iframe');
        iframe.src = '/font-picker.html?current=' + encodeURIComponent(current);
        iframe.style.cssText = 'flex:1;border:0;background:transparent;';
        box.appendChild(header);
        box.appendChild(iframe);
        overlay.appendChild(box);
        document.body.appendChild(overlay);
        const close = () => {
          window.removeEventListener('message', handler);
          if (overlay.parentNode) overlay.parentNode.removeChild(overlay);
        };
        const handler = (ev) => {
          const data = ev.data || {};
          if (data && data.type === 'font-picked' && data.cssValue) {
            if (target) {
              target.value = data.cssValue;
              target.dispatchEvent(new Event('input', { bubbles: true }));
            }
            close();
          }
        };
        window.addEventListener('message', handler);
        header.querySelector('#wtfk-font-close').addEventListener('click', close);
        overlay.addEventListener('click', (e) => { if (e.target === overlay) close(); });
      });
    });

    // Wire up collapses (close others when one opens)
    container.querySelectorAll('details.tpl-prop-section').forEach(det => {
      det.addEventListener('toggle', () => {
        if (!det.open) return;
        container.querySelectorAll('details.tpl-prop-section[open]').forEach(other => {
          if (other !== det) other.open = false;
        });
      });
    });
  }

  // Does any field have a showIf dependency on this key?
  function hasShowIfDeps(schema, key) {
    return (schema.fields || []).some(f => f.showIf && Object.keys(f.showIf).includes(key));
  }

  // Collect current props from the form
  function collectProps(container, schema) {
    const props = {};
    (schema.fields || []).forEach(f => {
      let el;
      if (f.type === 'radio') {
        el = container.querySelector(`input[type="radio"][data-wtfk="${f.key}"]:checked`);
      } else {
        el = container.querySelector(`[data-wtfk="${f.key}"]`);
      }
      if (!el) return;
      let val;
      switch (f.type) {
        case 'boolean':
          val = el.checked;
          break;
        case 'number':
        case 'range':
          val = el.value === '' ? '' : Number(el.value);
          break;
        default:
          val = el.value;
      }
      // Only include non-empty values
      if (val === '' || val === null || val === undefined) return;
      props[f.key] = val;
    });
    return props;
  }

  // Render the extras (key/value rows) inside extrasContainer
  function renderExtras(extrasContainer, schema, extras) {
    if (!extrasContainer) return;
    extrasContainer.innerHTML = `
      <div class="wtfk-extras-list">
        ${extras.map((row, i) => `
          <div class="wtfk-extras-row" data-wtfextras-idx="${i}">
            <input type="text" class="text-input" data-wtfextras-key="${i}"
                   value="${esc(row.key)}" placeholder="property name" style="flex:1;">
            <input type="text" class="text-input" data-wtfextras-value="${i}"
                   value="${esc(row.value)}" placeholder="value" style="flex:2;">
            <button type="button" class="btn btn-danger btn-sm" data-wtfextras-remove="${i}" title="Remove">✕</button>
          </div>`).join('')}
        <button type="button" class="btn btn-secondary btn-sm" id="wtfk-add-extra">+ Add Custom Property</button>
      </div>`;

    extrasContainer.querySelectorAll('[data-wtfextras-key]').forEach(el => {
      el.addEventListener('input', () => {
        const i = Number(el.dataset.wtfextrasKey);
        if (extras[i]) extras[i].key = el.value;
      });
    });
    extrasContainer.querySelectorAll('[data-wtfextras-value]').forEach(el => {
      el.addEventListener('input', () => {
        const i = Number(el.dataset.wtfextrasValue);
        if (extras[i]) extras[i].value = el.value;
      });
    });
    extrasContainer.querySelectorAll('[data-wtfextras-remove]').forEach(btn => {
      btn.addEventListener('click', () => {
        const i = Number(btn.dataset.wtfextrasRemove);
        extras.splice(i, 1);
        renderExtras(extrasContainer, schema, extras);
      });
    });
    const addBtn = extrasContainer.querySelector('#wtfk-add-extra');
    if (addBtn) {
      addBtn.addEventListener('click', () => {
        extras.push({ key: '', value: '' });
        renderExtras(extrasContainer, schema, extras);
      });
    }
  }

  // Load extras from a flat props object (separates defined fields from extras)
  function loadExtrasFromProps(schema, props) {
    const definedKeys = new Set((schema.fields || []).map(f => f.key));
    const extras = [];
    for (const k of Object.keys(props || {})) {
      if (!definedKeys.has(k)) {
        extras.push({ key: k, value: String(props[k]) });
      }
    }
    return extras;
  }

  // Collect extras into final props object
  function collectExtrasIntoProps(props, extras) {
    (extras || []).forEach(row => {
      if (row.key && row.key.trim()) {
        let parsed = row.value;
        // Try to parse as JSON for non-string values
        if (typeof row.value === 'string' && (row.value === 'true' || row.value === 'false' || /^-?\d/.test(row.value))) {
          try {
            const j = JSON.parse(row.value);
            if (typeof j === 'boolean' || typeof j === 'number') parsed = j;
          } catch (_) { /* keep as string */ }
        }
        props[row.key] = parsed;
      }
    });
    return props;
  }

  // Public API
  window.WidgetTemplateEditor = {
    renderForm,
    collectProps,
    renderExtras,
    loadExtrasFromProps,
    collectExtrasIntoProps,
  };
})();
