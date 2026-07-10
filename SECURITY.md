# Política de Segurança e Auditorias (SECURITY.md)

Este documento define a política de segurança do projeto **OffHeap** e detalha as auditorias de segurança contínuas implementadas para garantir a integridade e robustez da biblioteca.

---

## 🛡️ Versões Suportadas

Atualmente, apenas as seguintes versões recebem atualizações de segurança:

| Versão | Suportada |
| :--- | :---: |
| **0.3.x** | ✅ Sim (Ativa) |
| **< 0.3.0** | ❌ Não |

Recomendamos sempre manter a biblioteca atualizada na última versão minor/patch disponível para garantir que correções de segurança e otimizações estejam ativas em seu ambiente de produção.

> [!TIP]
> **Nota de Instalação:** Devido a problemas na publicação automatizada de pacotes nativos, as versões entre `0.3.0` e `0.3.10` podem falhar ao instalar em determinadas plataformas (como Windows e Linux musl). Recomendamos fortemente a utilização da versão `0.3.11` ou superior para ambiente funcional.

---

## 🔍 Auditorias de Segurança Automatizadas (CI)

Para garantir que nenhuma dependência vulnerável seja introduzida na biblioteca, executamos auditorias de segurança automáticas em cada *Push* e *Pull Request* no pipeline de Integração Contínua (CI), localizado em [.github/workflows/CI.yml](file:///f:/ryang/Development/principal/L1-Cache/.github/workflows/CI.yml).

As auditorias abrangem as duas principais camadas do projeto (Rust e Node.js):

### 1. Cargo Audit (Rust Crates)
*   **O que faz:** Varre o arquivo `Cargo.lock` contra a base de dados de vulnerabilidades conhecidas em Rust ([RustSec Advisory Database](https://rustsec.org)).
*   **Implementação no CI:**
    ```yaml
    - name: Run Cargo Audit
      uses: actions-rust-lang/audit@v1
    ```

### 2. NPM Audit (Node.js Dependencies)
*   **O que faz:** Analisa o arquivo `package-lock.json` contra vulnerabilidades conhecidas no ecossistema npm.
*   **Implementação no CI:**
    ```yaml
    - name: Run NPM Audit
      run: npm audit --omit=dev
    ```

> [!NOTE]  
> Ambas as auditorias rodam antes de qualquer compilação ou publicação. Se qualquer vulnerabilidade for encontrada em dependências de produção, o build falhará automaticamente e impedirá a publicação de novas versões do pacote no NPM.

---

## 🧠 Considerações de Segurança e Memória

Como o **OffHeap** gerencia dados diretamente na memória nativa (fora do heap do V8/Node.js), existem pontos importantes que desenvolvedores devem ter em mente ao usar a biblioteca:

### 1. Segurança de Memória com Rust
O núcleo do OffHeap é escrito em **Rust**. Nas partes do código que não usam `unsafe`, isso elimina por construção problemas como *buffer overflows*, *dangling pointers* e *data races*. 

Como o OffHeap cruza a fronteira FFI com o Node.js, partes do código exigem o uso de `unsafe` para gerenciar ponteiros brutos na ponte nativa. Essas seções passam por revisão manual criteriosa e testes dedicados (como validação de *panic safety* e contabilidade de memória sob concorrência) em vez de dependerem apenas das garantias estáticas do compilador.

O uso do alocador `mimalloc` garante que a gerência física de memória seja robusta, rápida e protegida contra fragmentação.

### 2. Descarte de Dados e Zeroização (Memory Zeroization)
*   Quando uma entrada do cache expira (TTL), é removida ou despejada por políticas de despejo (LRU, ARC, W-TinyLFU), seu espaço de memória é devolvido ao alocador nativo `mimalloc`.
*   Por padrão, o **OffHeap não sobrescreve os bytes com zeros (zeroization) antes de liberar a memória**.
*   **Recomendação:** Se você estiver armazenando dados extremamente sensíveis (ex: chaves privadas, senhas brutas), criptografe-os antes de inseri-los no cache ou certifique-se de realizar a limpeza (zeroização) a nível de aplicação.

### 3. Isolamento de Namespace e Risco de Colisão
*   Usar `CacheManager.createCache(name)` gera uma instância de cache física e logicamente isolada. As chaves não colidem entre namespaces de cache diferentes.
*   **Atenção:** Se sua aplicação decide unificar o cache e usar prefixos manuais para separar inquilinos (*tenants*), ex: `tenant_id + "::" + key`, certifique-se de higienizar/sanitizar as entradas para evitar ataques de colisão de chaves (*key collision attacks*).

### 4. Mitigações e Defesas Ativas contra Abuso e DoS
O OffHeap possui defesas explícitas integradas à arquitetura do código para conter vetores de ataque comuns em sistemas de cache:
*   **Proteção contra Integer Overflow**: Habilitamos explicitamente `overflow-checks = true` no perfil de release (`Cargo.toml`). Isso garante que operações aritméticas na contabilidade de tamanho e bytes (`bytes_used`) entrem em pânico controlado em caso de estouro em vez de falharem silenciosamente (o que poderia abrir brechas para corrupção de memória).
*   **Validação de Limite de Chaves**: Aplicamos uma validação rígida de que nenhuma chave de cache pode exceder o limite de segurança de **8192 bytes**. Essa checagem é feita de forma redundante: primeiro na camada JavaScript/TypeScript (para falhar rápido) e depois na camada Rust nativa.
*   **Resistência a Hash Flooding**: Para o roteamento de shards e a indexação interna do cache, o OffHeap utiliza a estrutura padrão `HashMap` do Rust, que emprega o algoritmo `SipHash-1-3` com sementes criptograficamente seguras e geradas aleatoriamente por processo (`RandomState`). Isso previne ataques de colisão proposital de hash criados para degradar a performance da tabela de hash para $O(N)$.
*   **Teto de Descompressão LZ4 Dinâmico**: Para evitar ataques do tipo *decompression bomb* (onde um payload compactado pequeno se expande em gigabytes na descompressão), o OffHeap valida o tamanho uncompressed do bloco LZ4 antes da alocação do vetor. Esse limite é dinâmico: ele é restrito a no máximo 32 MB ou a **10% do limite máximo de bytes da cache** (`maxBytes` * 0.1), o que for menor — com um piso mínimo de **1 KB** para evitar tetos degenerados em configurações de capacidade reduzida. Isso garante que a descompressão nunca cause exaustão súbita de memória (OOM).

---

## ✉️ Como Reportar uma Vulnerabilidade

Se você descobrir alguma vulnerabilidade de segurança no OffHeap, **por favor não abra uma Issue pública**. Em vez disso, siga o procedimento abaixo:

1. Envie um e-mail descrevendo detalhadamente a vulnerabilidade para o mantenedor: **ryangustav (através do GitHub ou e-mail de contato do projeto)** ou crie um **Draft Security Advisory** diretamente no repositório do GitHub.
2. Forneça o máximo de informações possível, incluindo:
   * Passos detalhados para reproduzir o problema.
   * Prova de Conceito (PoC) ou trecho de código que demonstre a vulnerabilidade.
   * Possível impacto no sistema.

Nós nos comprometemos a analisar o relatório rapidamente e responder com um plano de mitigação e correção dentro de um prazo adequado.
