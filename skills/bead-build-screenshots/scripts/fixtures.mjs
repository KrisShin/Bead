// 构建产物截图的状态清单。
// view 字段与 Rust StateView / web/render.js 对齐；所有译文均为固定 fixture 示例，
// 图片与 sidecar 会标注 sampleContent=true，不得当成实机翻译结果。

export const SAMPLE = {
  zh: '示例：改一张卡，其余语言自动同步。',
  en: 'Sample: edit one card and the rest sync automatically.',
  ja: '例：1枚のカードを編集すると、ほかの言語も自動で同期されます。',
  ko: '예시: 카드 하나를 수정하면 나머지 언어도 자동으로 동기화됩니다.',
  fr: 'Exemple : modifiez une carte et les autres langues se synchronisent.',
  de: 'Beispiel: Eine Karte bearbeiten, der Rest synchronisiert sich automatisch.',
  es: 'Ejemplo: edita una tarjeta y el resto se sincroniza automáticamente.',
  ru: 'Пример: измените одну карточку — остальные синхронизируются.',
};

const WIDTHS = { 2: 414, 3: 613, 4: 812, 5: 1011 };

function settings(overrides = {}) {
  return {
    auto_read_clipboard: true,
    hover_delay_ms: 200,
    leave_grace_ms: 800,
    selection_enabled: false,
    selection_hotkey: 'Ctrl+Alt+B',
    selection_hotkey_error: '',
    selection_watch_error: '',
    selection_anchor_supported: true,
    autostart_enabled: false,
    ocr_bundled_supported: true,
    ocr_installed: true,
    ocr_busy: false,
    ocr_engine: 'bundled',
    ocr_effective_engine: 'bundled',
    ocr_version: '1.0.0',
    ocr_total_size: 48234496,
    ocr_error: '',
    ...overrides,
  };
}

function provider(overrides = {}) {
  return {
    engine: 'zerokey',
    has_key: false,
    has_secret: false,
    needs_secret: false,
    model: '',
    base_url: '',
    ...overrides,
  };
}

function cards(langs, overrides = {}) {
  return langs.map((lang, index) => ({
    lang,
    text: overrides.empty ? '' : SAMPLE[lang],
    status: 'idle',
    authoritative: index === 0,
    ...(overrides.byLang?.[lang] ?? {}),
  }));
}

function view(overrides = {}) {
  return {
    view: 'expanded',
    locked: false,
    cards: cards(['zh', 'en']),
    settings: settings(),
    provider: provider(),
    languagesOpen: false,
    historyOpen: false,
    historyItems: [],
    ...overrides,
  };
}

const HISTORY = [
  {
    id: 3,
    snippet: '示例：改一张卡，其余语言自动同步。',
    targets: ['en', 'ja'],
    created_at_ms: 1763607600000,
  },
  {
    id: 2,
    snippet: '示例：悬停胶囊即可读取剪贴板并翻译。',
    targets: ['en', 'ja', 'ko'],
    created_at_ms: 1763604000000,
  },
  {
    id: 1,
    snippet: '示例：最多可以并排五种语言。',
    targets: ['en'],
    created_at_ms: 1763600400000,
  },
];

const panel = (count, overrides = {}) => ({
  width: WIDTHS[count],
  height: 300,
  view: view({ cards: cards(['zh', 'en', 'ja', 'ko', 'fr'].slice(0, count)), ...overrides }),
});

export const STATES = [
  {
    name: 'capsule-idle',
    title: '胶囊 · 收起',
    width: 68,
    height: 28,
    view: { ...view({ cards: [] }), view: 'capsule' },
    trigger: '构建产物 index.html + IPC stub 返回 view=capsule',
  },
  {
    name: 'capsule-hover',
    title: '胶囊 · 悬停',
    width: 68,
    height: 28,
    view: { ...view({ cards: [] }), view: 'capsule' },
    actions: [{ type: 'hover', selector: '#capsule' }],
    trigger: '同 capsule-idle，再用 CDP 移动真实鼠标到胶囊中心触发 :hover',
  },
  {
    name: 'panel-2',
    title: '展开面板 · 2 卡',
    ...panel(2),
    trigger: '初始快照直接给 2 卡；zh 为权威源，其余为 fixture 译文',
  },
  {
    name: 'panel-3',
    title: '展开面板 · 3 卡',
    ...panel(3),
    trigger: '初始快照给 3 卡',
  },
  {
    name: 'panel-4',
    title: '展开面板 · 4 卡',
    ...panel(4),
    trigger: '初始快照给 4 卡',
  },
  {
    name: 'panel-5',
    title: '展开面板 · 5 卡',
    ...panel(5),
    trigger: '初始快照给 5 卡（上限）',
  },
  {
    name: 'card-hover-close',
    title: '卡片 · 悬停显示关闭按钮',
    ...panel(3),
    actions: [{ type: 'hover', selector: '.card[data-lang="en"]' }],
    trigger: '3 卡时 canClose=true；CDP 悬停英文卡，显示右上角 ×',
  },
  {
    name: 'card-authoritative-en',
    title: '卡片 · 英文卡为权威源',
    ...panel(3, {
      cards: cards(['zh', 'en', 'ja'], { byLang: { zh: { authoritative: false }, en: { authoritative: true } } }),
    }),
    trigger: 'fixture 把 authoritative 放到 en，其余卡显示译文',
  },
  {
    name: 'card-syncing',
    title: '卡片 · 同步中',
    ...panel(3, {
      cards: cards(['zh', 'en', 'ja'], { byLang: { en: { status: 'syncing' } } }),
    }),
    trigger: 'fixture 给 en.status=syncing，渲染“同步中”标签',
  },
  {
    name: 'card-error',
    title: '卡片 · 同步失败',
    ...panel(3, {
      cards: cards(['zh', 'en', 'ja'], { byLang: { en: { status: 'error', text: '' } } }),
    }),
    trigger: 'fixture 给 en.status=error，渲染红框、“同步失败”和重试按钮',
  },
  {
    name: 'panel-empty',
    title: '展开面板 · 空文档',
    ...panel(2, { cards: cards(['zh', 'en'], { empty: true }) }),
    trigger: 'fixture 卡片文本为空，展示空态布局',
  },
  {
    name: 'panel-locked',
    title: '展开面板 · 锁定',
    ...panel(3, { locked: true }),
    trigger: 'fixture locked=true，锁定图标呈按下态',
  },
  {
    name: 'settings-general',
    title: '设置 · 顶部（provider 与通用开关）',
    ...panel(3, { languagesOpen: true }),
    actions: [
      { type: 'click', selector: '[data-action="languages"]' },
      { type: 'scroll', selector: '.board__settings', position: 'top' },
    ],
    trigger: '点击头部设置按钮展开设置；滚动到设置区顶部',
  },
  {
    name: 'settings-provider-deepl',
    title: '设置 · provider 选中 DeepL',
    ...panel(3, {
      languagesOpen: true,
      provider: provider({ engine: 'deepl', has_key: true, has_secret: false }),
    }),
    actions: [
      { type: 'click', selector: '[data-action="languages"]' },
      { type: 'scroll', selector: '.board__settings', position: 'top' },
    ],
    trigger: 'fixture provider.engine=deepl 且 has_key=true',
  },
  {
    name: 'settings-bottom',
    title: '设置 · 底部（TTS / OCR / 历史 / 更新）',
    ...panel(3, { languagesOpen: true }),
    actions: [
      { type: 'click', selector: '[data-action="languages"]' },
      { type: 'scroll', selector: '.board__settings', position: 'bottom' },
    ],
    trigger: '点击头部设置按钮展开设置；滚动到设置区底部',
  },
  {
    name: 'settings-ocr-bundled',
    title: '设置 · OCR 内置已安装并选中',
    ...panel(3, {
      languagesOpen: true,
      settings: settings({ ocr_installed: true, ocr_engine: 'bundled', ocr_effective_engine: 'bundled' }),
    }),
    actions: [
      { type: 'click', selector: '[data-action="languages"]' },
      { type: 'scroll', selector: '.board__settings', position: 'bottom' },
    ],
    trigger: 'fixture ocr_installed=true、ocr_engine=bundled',
  },
  {
    name: 'settings-ocr-system',
    title: '设置 · OCR 系统方案选中',
    ...panel(3, {
      languagesOpen: true,
      settings: settings({ ocr_installed: true, ocr_engine: 'system', ocr_effective_engine: 'system' }),
    }),
    actions: [
      { type: 'click', selector: '[data-action="languages"]' },
      { type: 'scroll', selector: '.board__settings', position: 'bottom' },
    ],
    trigger: 'fixture ocr_engine=system，显示“体验不佳”与选中态',
  },
  {
    name: 'settings-ocr-not-installed',
    title: '设置 · OCR 未安装',
    ...panel(3, {
      languagesOpen: true,
      settings: settings({ ocr_installed: false, ocr_engine: 'bundled', ocr_effective_engine: 'system' }),
    }),
    actions: [
      { type: 'click', selector: '[data-action="languages"]' },
      { type: 'scroll', selector: '.board__settings', position: 'bottom' },
    ],
    trigger: 'fixture ocr_installed=false，显示下载体积与安装按钮',
  },
  {
    name: 'settings-ocr-busy',
    title: '设置 · OCR 下载中',
    ...panel(3, {
      languagesOpen: true,
      settings: settings({ ocr_installed: false, ocr_busy: true }),
    }),
    actions: [
      { type: 'click', selector: '[data-action="languages"]' },
      { type: 'scroll', selector: '.board__settings', position: 'bottom' },
      {
        type: 'ocrProgress',
        payload: { file: 'ppocr-v4-det.tar.gz', total: 100, overall: 46, phase: 'downloading' },
      },
    ],
    trigger: 'fixture ocr_busy=true，再推送 ocr-progress 事件填充进度条',
  },
  {
    name: 'settings-ocr-error',
    title: '设置 · OCR 错误',
    ...panel(3, {
      languagesOpen: true,
      settings: settings({ ocr_installed: false, ocr_error: '示例错误：下载校验失败，请重试。' }),
    }),
    actions: [
      { type: 'click', selector: '[data-action="languages"]' },
      { type: 'scroll', selector: '.board__settings', position: 'bottom' },
    ],
    trigger: 'fixture ocr_error 非空，显示红色错误提示',
  },
  {
    name: 'settings-hotkey-error',
    title: '设置 · 非法快捷键',
    ...panel(3, {
      languagesOpen: true,
      settings: settings({ selection_hotkey: 'Ctrl+Alt+NotAKey', selection_hotkey_error: '无法解析快捷键：请使用 Ctrl/Alt/Shift 加一个按键。' }),
    }),
    actions: [
      { type: 'click', selector: '[data-action="languages"]' },
      { type: 'scroll', selector: '.board__settings', position: 'top' },
    ],
    trigger: 'fixture selection_hotkey_error 非空',
  },
  {
    name: 'settings-watch-error',
    title: '设置 · 划词监听失败',
    ...panel(3, {
      languagesOpen: true,
      settings: settings({ selection_enabled: true, selection_watch_error: '示例错误：全局快捷键被其他程序占用。' }),
    }),
    actions: [
      { type: 'click', selector: '[data-action="languages"]' },
      { type: 'scroll', selector: '.board__settings', position: 'top' },
    ],
    trigger: 'fixture selection_watch_error 非空',
  },
  {
    name: 'settings-anchor-unsupported',
    title: '设置 · 划词图标不可用',
    ...panel(3, {
      languagesOpen: true,
      settings: settings({ selection_anchor_supported: false }),
    }),
    actions: [
      { type: 'click', selector: '[data-action="languages"]' },
      { type: 'scroll', selector: '.board__settings', position: 'top' },
    ],
    trigger: 'fixture selection_anchor_supported=false，显示平台不支持提示',
  },
  {
    name: 'history-list',
    title: '翻译历史 · 有记录',
    ...panel(3, { historyOpen: true, historyItems: HISTORY }),
    actions: [{ type: 'click', selector: '[data-action="history"]' }],
    trigger: '点击头部历史按钮；list_history 返回 3 条固定示例',
  },
  {
    name: 'history-empty',
    title: '翻译历史 · 空态',
    ...panel(3, { historyOpen: true, historyItems: [] }),
    actions: [{ type: 'click', selector: '[data-action="history"]' }],
    trigger: '点击头部历史按钮；list_history 返回空数组',
  },
  {
    name: 'selection-on',
    title: '划词开关 · 开启',
    ...panel(3, { settings: settings({ selection_enabled: true }) }),
    trigger: 'fixture settings.selection_enabled=true，header 划词图标为按下态',
  },
  {
    name: 'ocr-thumb-pending',
    title: '卡片 · 粘贴缩略图（识别中）',
    ...panel(2),
    ocrMode: 'pending',
    actions: [{ type: 'paste', lang: 'zh', waitMs: 400 }],
    trigger: '合成 paste 事件插入示例图片；OCR IPC 返回挂起 Promise，保持“识别中…”',
  },
  {
    name: 'ocr-thumb-empty',
    title: '卡片 · 粘贴缩略图（未识别到文字）',
    ...panel(2),
    ocrMode: 'empty',
    actions: [{ type: 'paste', lang: 'zh', waitMs: 800 }],
    trigger: '合成 paste 事件；OCR IPC 返回空字符串',
  },
  {
    name: 'ocr-thumb-failed',
    title: '卡片 · 粘贴缩略图（识别失败）',
    ...panel(2),
    ocrMode: 'fail',
    actions: [{ type: 'paste', lang: 'zh', waitMs: 800 }],
    trigger: '合成 paste 事件；OCR IPC 抛错',
  },
];

// 前端包渲染无法覆盖、或后端永远不会产生的状态；索引里明确标注，不出图。
export const NOT_CAPTURABLE = [
  {
    name: 'panel-edge-clamp',
    title: '贴边 clamp',
    reason: '窗口位置与 clamp 由 Rust 计算；前端包渲染里没有屏幕边缘坐标，无法从画面体现。',
  },
  {
    name: 'card-drag',
    title: '卡片拖拽中',
    reason: '需要按住 HTML5 drag 的瞬时交互，headless 静态渲染无稳定中间帧。',
  },
  {
    name: 'panel-animation',
    title: '展开/收起动画中间帧',
    reason: '过渡由窗口 resize 与内容补间共同驱动，单帧截图不具代表性。',
  },
  {
    name: 'selection-anchor',
    title: '划词选区气泡',
    reason: '独立锚点窗口，需要真实选区与系统级光标；不在主 web 包内。',
  },
  {
    name: 'update-badge',
    title: '更新徽标',
    reason: '后端 update_latest 只有初始化和读取，没有任何写入路径，实际不可达。',
  },
  {
    name: 'provider-test',
    title: '测试连接结果',
    reason: 'test_provider 明确不回写任何界面状态。',
  },
];
