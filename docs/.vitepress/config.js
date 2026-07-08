export default {
  title: 'OffHeap',
  description: 'High-performance off-heap caching framework for Node.js',
  themeConfig: {
    logo: '/logo.png',
    nav: [
      { text: 'Guide', link: '/guide/getting-started' },
      { text: 'API Reference', link: '/guide/api' },
      { text: 'Benchmarks', link: '/guide/benchmarks' }
    ],
    sidebar: [
      {
        text: 'Getting Started',
        items: [
          { text: 'Introduction', link: '/guide/getting-started' },
          { text: 'Architecture & Design', link: '/guide/architecture' }
        ]
      },
      {
        text: 'Reference',
        items: [
          { text: 'API Reference', link: '/guide/api' },
          { text: 'Benchmarks & Telemetry', link: '/guide/benchmarks' }
        ]
      }
    ],
    socialLinks: [
      { icon: 'github', link: 'https://github.com/ryangustav/OffHeap' }
    ],
    footer: {
      message: 'Released under the MIT and Apache 2.0 Licenses.',
      copyright: 'Copyright © 2026 Ryan Gustavo & OffHeap Contributors'
    }
  }
}
