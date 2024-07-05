---
sidebar_position: 12
---

# FAQ

:::tip

[Open an issue](https://github.com/Qovery/replibyte/issues/new) if you don't find the answer to your question.

:::

### What language is used for Replibyte?

[Rust](https://www.rust-lang.org/)

### Why using Rust?

Replibyte is a IO intensive tool that need to process data as fast as possible. Rust is a perfect candidate for high throughput and low
memory consumption.

### Does RepliByte is an ETL?

RepliByte is not an ETL like [AirByte](https://github.com/airbytehq/airbyte), [AirFlow](https://airflow.apache.org/), Talend, and it will
never be. If you need to synchronize versatile data sources, you are better choosing a classic ETL. RepliByte is a tool for software
engineers to help them to synchronize data from the same databases. With RepliByte, you can only replicate data from the same type of
databases. As mentioned above, the primary purpose of RepliByte is to duplicate into different environments. You can see RepliByte as a
specific use case of an ETL, where an ETL is more generic.

### Do you support backup from a dump file?

absolutely,

```shell
cat dump.sql | replibyte -c conf.yaml dump create -s postgres -i
```

and

```shell
replibyte -c conf.yaml dump create -s postgres -f dump.sql
```

### How RepliByte can list the dumps? Is there an API?

There is no API, RepliByte is fully stateless and store the dump list into the datastore (E.g. S3) via an metadata file.

### How can I contact you?

3 options:

1. [Open an issue](https://github.com/Qovery/replibyte/issues/new).
2. Join our #replibyte channel on [our discord](https://discord.qovery.com).
3. Drop us an email to `github+replibyte {at} qovery {dot} com`.
