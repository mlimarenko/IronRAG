export default {
  brand: {
    eyebrow: 'RustRAG',
    title: 'RustRAG',
    subtitle: 'Поиск и ответы по вашим файлам.',
    badge: 'Preview',
  },
  nav: {
    primary: 'Разделы',
    manage: 'Настройка',
    items: {
      files: {
        label: 'Files',
        caption: 'Добавление и просмотр файлов',
      },
      processing: {
        label: 'Processing',
        caption: 'Статус и следующие шаги',
      },
      search: {
        label: 'Search',
        caption: 'Вопросы по вашим файлам',
      },
      graph: {
        label: 'Graph',
        caption: 'Связи и сигналы знаний',
      },
      api: {
        label: 'API',
        caption: 'Интеграции поверх тех же данных',
      },
      setup: {
        label: 'Setup',
        caption: 'Выбор пространства и коллекции',
      },
    },
  },
  topbar: {
    surface: 'Раздел',
    language: 'Язык',
    state: 'Статус',
  },
  locale: {
    en: 'EN',
    ru: 'RU',
  },
  status: {
    focused: 'Фокус',
    ready: 'Готово',
    healthy: 'Стабильно',
  },
  pages: {
    files: {
      title: 'Files',
    },
    processing: {
      title: 'Processing',
    },
    setup: {
      title: 'Setup',
    },
    search: {
      title: 'Search',
    },
    graph: {
      title: 'Graph',
    },
    api: {
      title: 'API',
    },
  },
} as const
