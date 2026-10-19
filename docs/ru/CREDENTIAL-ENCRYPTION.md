<div align="center">

# Шифрование учётных данных

### Обязательное шифрование и ротация главного ключа

[Обзор](./README.md) | [English](../en/CREDENTIAL-ENCRYPTION.md) | [Webhooks](./WEBHOOK.md) | [AI bindings](./AI-BINDINGS.md)

</div>

## Модель безопасности

IronRAG хранит учётные данные AI-аккаунтов, секреты подписи исходящих webhook и
значения дополнительных заголовков в аутентифицированных, привязанных к строке
конвертах `ironrag:enc:v3`. Главный ключ хранится вне PostgreSQL и резервируется
отдельно.

Шифрование учётных данных **обязательно и всегда включено**:

- `IRONRAG_CREDENTIAL_MASTER_KEY` и keyring обязательны. API и worker
  **не стартуют** без валидного ключа (fail closed); отключённого или
  read-only режима нет.
- Каждый сохранённый credential — это v3 envelope. Сохранённое значение, не являющееся v3
  envelope, закрывается fail-closed: оно отвергается при чтении и помечается
  inventory как invalid, а не расшифровывается.
- Чтение работает для активного v3 key и любых настроенных previous v3 keys.
  Неизвестный v3 key ID приводит к fail-closed ошибке.

`install.sh` генерирует master key при создании новой `.env` и добавляет его в
существующую `.env` без ключа. Docker Compose и Helm
требуют, чтобы ключ был задан (inline или через внешний Secret).

## Поведение Helm rollout

Для ConfigMap и Secret, которые рендерит chart, checksum-аннотации автоматически
перезапускают API, worker и startup pods при изменении runtime environment.

Helm не может посчитать checksum Secret, указанного через
`runtimeSecret.existingSecret`, поэтому вместе с ним обязателен непустой
`runtimeSecret.restartNonce`. Меняйте nonce при каждом изменении внешнего Secret.
Повторное использование nonce оставит запущенные pods со старыми env-значениями,
даже если объект Secret уже изменился.

```yaml
runtimeSecret:
  existingSecret: ironrag-runtime-production
  restartNonce: "credential-rotation-phase-1-2026-07-10"
```

Используйте новый nonce для каждой фазы ротации ключа.

## Трёхфазная ротация master key

Key ID содержит 1-32 строчные латинские буквы, цифры, `.`, `_` или `-` и
начинается с буквы или цифры. `IRONRAG_CREDENTIAL_PREVIOUS_MASTER_KEYS`
поддерживает до восьми уникальных записей `id=padded-standard-base64-key` без
пробелов, строго отсортированных по key ID. Previous entry используется только для
decrypt/rewrap.

Пусть `key-2026-01` — активный ключ, а `key-2026-07` — новый.

### Фаза 1: раздайте новый ключ без переключения active

Оставьте старый ключ active. Добавьте новый в previous-key map и разверните этот
bridging keyring на каждом API и worker:

```env
IRONRAG_CREDENTIAL_MASTER_KEY_ID=key-2026-01
IRONRAG_CREDENTIAL_MASTER_KEY=<old-key>
IRONRAG_CREDENTIAL_PREVIOUS_MASTER_KEYS=key-2026-07=<new-key>
```

До продолжения убедитесь, что каждый replica успешно перезапущен. Несмотря на
название `previous`, размещение нового ключа здесь делает следующий rolling
overlap безопасным.

### Фаза 2: переключите active key

Сделайте новый ключ active, а старый оставьте как previous:

```env
IRONRAG_CREDENTIAL_MASTER_KEY_ID=key-2026-07
IRONRAG_CREDENTIAL_MASTER_KEY=<new-key>
IRONRAG_CREDENTIAL_PREVIOUS_MASTER_KEYS=key-2026-01=<old-key>
```

Во время rollout replica со старым config расшифровывают записи новым ключом,
потому что получили его на фазе 1. Replica с новым config расшифровывают записи
старым ключом через previous-key map.

### Фаза 3: rewrap, проверка и удаление старого ключа

Выполните последовательность dry/apply/dry. Удаляйте старый ключ только когда:

- итоговый inventory не содержит old-key строк;
- каждый API и worker использует новый active key;
- backup БД с old-key envelope истёк согласно retention policy; и
- restore drill подтвердил соответствие сохранённых backup и key material.

После этого удалите старую запись, разверните сокращённый keyring на каждом
replica и снова проверьте readiness. Не уничтожайте единственную копию старого
ключа сразу: храните её согласно утверждённой политике retention и уничтожения.
