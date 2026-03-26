# Purser

A sovereign, Nostr-native checkout daemon that replaces Zaprite. Purser sits on your own hardware (Raspberry Pi, mini-PC, VPS) and handles the entire payment flow — from encrypted customer orders to payment processing to confirmation — without exposing any public endpoints.

All merchant-customer communication is end-to-end encrypted via MLS (using [marmot-protocol/mdk](https://github.com/marmot-protocol/mdk)), and payment provider integrations are pluggable via a common trait. V1 ships with Square (fiat/cards) and Strike (Bitcoin/Lightning).

## Why "Purser"?

The name comes from the officer historically responsible for managing payments and provisions aboard ships. Dating back to at least the 13th century, the purser (from Anglo-French *bursier*, "keeper of the purse") served as the financial steward on naval and merchant vessels — handling all monetary transactions, maintaining accounts, and ensuring that goods were paid for and delivered. The role carried significant trust: the purser was personally accountable for every coin that passed through the ship's stores.

Like its namesake, this daemon is a single trusted agent that sits between your customers and your payment processors, accountable for every transaction that flows through it.
