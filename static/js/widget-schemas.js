// Widget property schemas for the object-based template editor.
// Each entry describes a single prop's UI + validation. The editor renders
// form controls from these descriptors, so adding a new widget type or a
// new prop is just adding an entry here.
//
// Field shape:
//   key       (string, required)   JSON prop key stored in widget.props
//   label     (string, required)   UI label
//   type      (string, required)   one of: string, text, number, range, boolean,
//                                  color, url, file, select, radio
//   section   (string, required)   which accordion section this field belongs to
//   default   (any, optional)      value used when creating a new template
//   required  (boolean, optional)  show red asterisk
//   hint      (string, optional)   shown to the right of the field
//   help      (string, optional)   shown under the field (small grey text)
//   min       (number, optional)   for number/range
//   max       (number, optional)   for number/range
//   step      (number, optional)   for number/range
//   options   (array, optional)    for select/radio: [{value, label}]
//   showIf    (object, optional)   only show if all specified props match
//                                  e.g. {template: 'list'} shows only when
//                                  props.template === 'list'
//   placeholder (string, optional) for text/url inputs
//   extensions  (array, optional)  for file: ['.png', '.jpg', '.svg']
//   browse     (string, optional)  for url/text: 'file' opens file picker;
//                                  'fonts' opens font chooser; undefined = plain input

window.WIDGET_SCHEMAS = {
  text: {
    label: 'Text',
    icon: '📝',
    sections: [
      { id: 'content', label: 'Content', icon: '📄' },
      { id: 'font',    label: 'Font',    icon: '🎨' },
      { id: 'background', label: 'Background', icon: '🖼️' },
      { id: 'layout',  label: 'Layout',  icon: '📐' },
    ],
    fields: [
      { key: 'text', label: 'Text', type: 'text', section: 'content', default: 'Hello, world!', required: true, hint: 'Shown on screen' },
      { key: 'bold', label: 'Bold', type: 'boolean', section: 'content', default: false, help: 'Render in bold weight' },
      { key: 'font_family', label: 'Font Family', type: 'string', section: 'font', placeholder: '-apple-system, sans-serif', hint: 'CSS font-family', browse: 'fonts' },
      { key: 'font_size', label: 'Font Size', type: 'number', section: 'font', default: 24, min: 6, max: 200, step: 1, hint: 'px' },
      { key: 'font_weight', label: 'Font Weight', type: 'select', section: 'font', default: 400,
        options: [{value: 300, label: 'Light (300)'}, {value: 400, label: 'Normal (400)'}, {value: 500, label: 'Medium (500)'}, {value: 700, label: 'Bold (700)'}, {value: 900, label: 'Black (900)'}] },
      { key: 'bg_color', label: 'Background Color', type: 'color', section: 'background', default: '#000000' },
      { key: 'bg_opacity', label: 'Background Opacity', type: 'range', section: 'background', default: 0.5, min: 0, max: 1, step: 0.05, hint: '0 = transparent, 1 = opaque' },
      { key: 'align', label: 'Text Alignment', type: 'radio', section: 'layout', default: 'center',
        options: [{value: 'left', label: 'Left'}, {value: 'center', label: 'Center'}, {value: 'right', label: 'Right'}] },
      { key: 'padding', label: 'Padding', type: 'number', section: 'layout', default: 8, min: 0, max: 100, hint: 'px' },
      { key: 'border_radius', label: 'Border Radius', type: 'number', section: 'layout', default: 0, min: 0, max: 100, hint: 'px' },
    ],
  },

  image: {
    label: 'Image',
    icon: '🖼️',
    sections: [
      { id: 'source', label: 'Source', icon: '📁' },
      { id: 'accessibility', label: 'Accessibility', icon: '♿' },
    ],
    fields: [
      { key: 'url', label: 'Image URL', type: 'url', section: 'source', required: true, hint: 'http://... or /uploads/...', browse: 'file',
        help: 'Any image URL. Use a relative path under /uploads/ to use a local file.' },
      { key: 'alt', label: 'Alt Text', type: 'string', section: 'accessibility', placeholder: 'Description for screen readers', help: 'Improves accessibility.' },
    ],
  },

  video: {
    label: 'Video',
    icon: '🎬',
    sections: [
      { id: 'source', label: 'Source', icon: '📁' },
      { id: 'audio', label: 'Audio', icon: '🔊' },
      { id: 'priority', label: 'Priority', icon: '⚡' },
    ],
    fields: [
      { key: 'url', label: 'Video URL', type: 'url', section: 'source', required: true, hint: 'YouTube or direct .mp4', browse: 'file',
        help: 'YouTube URLs render in an iframe; .mp4/.webm use the <video> tag.' },
      { key: 'mute', label: 'Mute by Default', type: 'boolean', section: 'audio', default: true, help: 'Required for autoplay on most browsers.' },
      { key: 'priority', label: 'Sound Priority', type: 'number', section: 'priority', default: 50, min: 0, max: 100,
        help: 'Higher-priority sources duck lower-priority ones. 0-49 = background, 50-99 = primary, 100 = alert.' },
    ],
  },

  clock: {
    label: 'Clock',
    icon: '🕐',
    sections: [
      { id: 'font', label: 'Font', icon: '🎨' },
      { id: 'format', label: 'Format', icon: '🕒' },
    ],
    fields: [
      { key: 'font_family', label: 'Font Family', type: 'string', section: 'font', placeholder: 'sans-serif', hint: 'CSS font-family', browse: 'fonts' },
      { key: 'font_size', label: 'Font Size', type: 'number', section: 'font', default: 96, min: 12, max: 400, step: 1, hint: 'px' },
      { key: 'font_weight', label: 'Font Weight', type: 'select', section: 'font', default: 700,
        options: [{value: 300, label: 'Light'}, {value: 400, label: 'Normal'}, {value: 500, label: 'Medium'}, {value: 700, label: 'Bold'}, {value: 900, label: 'Black'}] },
      { key: 'color', label: 'Text Color', type: 'color', section: 'font', default: '#ffffff' },
      { key: 'format', label: 'Time Format', type: 'radio', section: 'format', default: '24h',
        options: [{value: '24h', label: '24-hour (14:30)'}, {value: '12h', label: '12-hour (2:30 PM)'}] },
    ],
  },

  datetime: {
    label: 'Date',
    icon: '📅',
    sections: [
      { id: 'font', label: 'Font', icon: '🎨' },
      { id: 'format', label: 'Format', icon: '🗓️' },
    ],
    fields: [
      { key: 'font_family', label: 'Font Family', type: 'string', section: 'font', placeholder: 'sans-serif', hint: 'CSS font-family', browse: 'fonts' },
      { key: 'font_size', label: 'Font Size', type: 'number', section: 'font', default: 36, min: 12, max: 200, hint: 'px' },
      { key: 'color', label: 'Text Color', type: 'color', section: 'font', default: '#ffffff' },
      { key: 'format', label: 'Date Style', type: 'select', section: 'format', default: 'dddd, MMMM D',
        options: [
          {value: 'dddd, MMMM D', label: 'Long (Monday, January 1)'},
          {value: 'MMMM D, YYYY', label: 'Medium (January 1, 2026)'},
          {value: 'MM/DD/YYYY', label: 'Short (01/01/2026)'},
        ] },
    ],
  },

  calendar: {
    label: 'Calendar',
    icon: '📆',
    sections: [
      { id: 'weather', label: 'Weather Strip', icon: '🌤️' },
    ],
    fields: [
      { key: 'include_weather', label: 'Show Weather Strip', type: 'boolean', section: 'weather', default: false },
      { key: 'lat', label: 'Latitude', type: 'number', section: 'weather', default: 0, step: 0.0001, min: -90, max: 90, showIf: { include_weather: true } },
      { key: 'lon', label: 'Longitude', type: 'number', section: 'weather', default: 0, step: 0.0001, min: -180, max: 180, showIf: { include_weather: true } },
    ],
  },

  weather: {
    label: 'Weather',
    icon: '🌤️',
    sections: [
      { id: 'units', label: 'Units', icon: '🌡️' },
      { id: 'color', label: 'Color', icon: '🌈' },
    ],
    fields: [
      { key: 'unit', label: 'Temperature', type: 'radio', section: 'units', default: 'f',
        options: [{value: 'f', label: 'Fahrenheit (°F)'}, {value: 'c', label: 'Celsius (°C)'}] },
      { key: 'color', label: 'Text Color', type: 'color', section: 'color', default: '#ffffff' },
      { key: 'accent', label: 'Accent Color', type: 'color', section: 'color', default: '#4a90d9' },
    ],
  },

  webpage: {
    label: 'Webpage',
    icon: '🌐',
    sections: [
      { id: 'source', label: 'Source', icon: '📁' },
      { id: 'timing', label: 'Timing', icon: '⏱️' },
    ],
    fields: [
      { key: 'web_type', label: 'Type', type: 'select', section: 'source', default: 'normal',
        options: [{value: 'normal', label: 'Normal — embed URL in iframe'}, {value: 'google_slides', label: 'Google Slides — auto-reload'}],
        help: 'Google Slides mode auto-reloads every slide_duration seconds.' },
      { key: 'url', label: 'URL', type: 'url', section: 'source', required: true, hint: 'https://...', browse: 'file' },
      { key: 'slide_duration', label: 'Slide Duration', type: 'number', section: 'timing', default: 10, min: 3, max: 3600, hint: 'seconds', showIf: { web_type: 'google_slides' } },
    ],
  },

  music: {
    label: 'Music',
    icon: '🎵',
    sections: [
      { id: 'source', label: 'Source', icon: '📁' },
      { id: 'audio', label: 'Audio', icon: '🔊' },
      { id: 'priority', label: 'Priority', icon: '⚡' },
    ],
    fields: [
      { key: 'url', label: 'Audio URL', type: 'file', section: 'source', required: true, hint: '.mp3, .ogg, .wav',
        extensions: ['.mp3', '.ogg', '.wav', '.m4a'], browse: 'file' },
      { key: 'mute', label: 'Muted', type: 'boolean', section: 'audio', default: false },
      { key: 'volume', label: 'Volume', type: 'range', section: 'audio', default: 0.7, min: 0, max: 1, step: 0.05, hint: '0 = silent, 1 = full' },
      { key: 'priority', label: 'Sound Priority', type: 'number', section: 'priority', default: 30, min: 0, max: 100,
        help: 'Higher-priority sources duck lower-priority ones. 0-29 = background music.' },
    ],
  },

  rectangle: {
    label: 'Rectangle',
    icon: '🟥',
    sections: [
      { id: 'border', label: 'Border', icon: '🔲' },
    ],
    fields: [
      { key: 'radius', label: 'Corner Radius', type: 'number', section: 'border', default: 0, min: 0, max: 100, hint: 'px' },
    ],
  },

  // News Ticker. Added so users can build custom news layouts in the
  // Widget Templates editor (the four 'complex' widgets the editor
  // is intended for: Weather, Calendar, News, Embed). The props
  // match the existing makeLayoutWidget('news') defaults in editor.js.
  news: {
    label: 'News Ticker',
    icon: '📰',
    sections: [
      { id: 'source', label: 'Source', icon: '📜' },
      { id: 'display', label: 'Display', icon: '🎬' },
    ],
    fields: [
      { key: 'feedUrl', label: 'RSS / Atom Feed URL', type: 'url', section: 'source', required: true,
        placeholder: 'https://example.com/rss.xml', hint: 'Source of headlines' },
      { key: 'speed', label: 'Scroll Speed', type: 'select', section: 'display', default: 'normal',
        options: [
          { value: 'slow', label: 'Slow' },
          { value: 'normal', label: 'Normal' },
          { value: 'fast', label: 'Fast' },
        ],
        hint: 'How fast the ticker scrolls' },
    ],
  },
};

// Helper: get schema for a widget type, with fallback for unknown types
window.getWidgetSchema = function (widgetType) {
  return window.WIDGET_SCHEMAS[widgetType] || {
    label: widgetType,
    icon: '❓',
    sections: [{ id: 'misc', label: 'Properties', icon: '🔧' }],
    fields: [],
  };
};

// Helper: returns true if a field should be visible given current props
window.fieldIsVisible = function (field, currentProps) {
  if (!field.showIf) return true;
  for (const [k, v] of Object.entries(field.showIf)) {
    if (currentProps[k] !== v) return false;
  }
  return true;
};
