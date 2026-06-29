#![allow(unused)]
use bitcoin::hex::DisplayHex;
use bitcoincore_rpc::bitcoin::{Amount, SignedAmount};
use bitcoincore_rpc::{Auth, Client, RpcApi};
use serde::Deserialize;
use serde_json::json;
use std::fs::File;
use std::io::Write;

// Node access params
const RPC_URL: &str = "http://127.0.0.1:18443"; // Default regtest RPC port
const RPC_USER: &str = "alice";
const RPC_PASS: &str = "password";

// You can use calls not provided in RPC lib API using the generic `call` function.
fn send(rpc: &Client, addr: &str) -> bitcoincore_rpc::Result<String> {
    let args = [
        json!([{addr : 100 }]),
        json!(null),
        json!(null),
        json!(null),
        json!(null),
    ];

    #[derive(Deserialize)]
    struct SendResult {
        complete: bool,
        txid: String,
    }
    let send_result = rpc.call::<SendResult>("send", &args)?;
    assert!(send_result.complete);
    Ok(send_result.txid)
}

/// Helper: returns an RPC client scoped to a specific wallet,
/// e.g. http://127.0.0.1:18443/wallet/Miner
fn wallet_client(wallet_name: &str) -> bitcoincore_rpc::Result<Client> {
    let url = format!("{}/wallet/{}", RPC_URL, wallet_name);
    Client::new(
        &url,
        Auth::UserPass(RPC_USER.to_owned(), RPC_PASS.to_owned()),
    )
}

/// Creates a wallet if it doesn't exist yet, or loads it if it exists but isn't loaded.
/// Bitcoin Core errors if you try to create a wallet that already exists on disk,
/// or load one that's already loaded — so we handle both cases gracefully.
fn create_or_load_wallet(rpc: &Client, wallet_name: &str) -> bitcoincore_rpc::Result<()> {
    match rpc.create_wallet(wallet_name, None, None, None, None) {
        Ok(_) => {
            println!("Created wallet: {}", wallet_name);
            Ok(())
        }
        Err(_) => {
            // Wallet likely already exists on disk; try loading it instead.
            match rpc.load_wallet(wallet_name) {
                Ok(_) => {
                    println!("Loaded existing wallet: {}", wallet_name);
                    Ok(())
                }
                Err(e) => {
                    println!(
                        "Wallet '{}' likely already loaded, continuing. ({})",
                        wallet_name, e
                    );
                    Ok(())
                }
            }
        }
    }
}

fn main() -> bitcoincore_rpc::Result<()> {
    // Connect to Bitcoin Core RPC (base node client, no wallet attached yet)
    let rpc = Client::new(
        RPC_URL,
        Auth::UserPass(RPC_USER.to_owned(), RPC_PASS.to_owned()),
    )?;

    // Get blockchain info
    let blockchain_info = rpc.get_blockchain_info()?;
    println!("Blockchain Info: {:?}", blockchain_info);

    // Create/Load the 'Miner' and 'Trader' wallets

    create_or_load_wallet(&rpc, "Miner")?;
    create_or_load_wallet(&rpc, "Trader")?;

    let miner_rpc = wallet_client("Miner")?;
    let trader_rpc = wallet_client("Trader")?;

    // Generate a Miner address labeled "Mining Reward"

    let mining_address = miner_rpc.get_new_address(Some("Mining Reward"), None)?;
    let mining_address = mining_address.assume_checked();

    let mut blocks_mined = 0;
    loop {
        rpc.generate_to_address(1, &mining_address)?;
        blocks_mined += 1;
        let balance = miner_rpc.get_balance(None, None)?;
        if balance > Amount::ZERO {
            println!(
                "Positive balance ({} BTC) reached after mining {} blocks.",
                balance, blocks_mined
            );
            // Balance stays at 0 immediately after mining because the
            // coinbase reward of the very first block is "immature" —
            // it requires 100 confirmations (100 additional blocks on
            // top of it) before Bitcoin Core counts it as spendable
            // wallet balance. So it takes 101 mined blocks total
            // (1 block containing the reward + 100 confirming blocks)
            // before get_balance() returns anything > 0.
            break;
        }
    }

    //  Print Miner's balance
    let miner_balance = miner_rpc.get_balance(None, None)?;
    println!("Miner wallet balance: {} BTC", miner_balance);

    // Create a Trader receiving address labeled "Received"

    let trader_address = trader_rpc.get_new_address(Some("Received"), None)?;
    let trader_address = trader_address.assume_checked();

    // Step 6: Send 20 BTC from Miner to Trader

    let txid = miner_rpc.send_to_address(
        &trader_address,
        Amount::from_btc(20.0).unwrap(),
        None,
        None,
        None,
        None,
        None,
        None,
    )?;
    println!("Sent 20 BTC to Trader. txid: {}", txid);

    //Fetch the unconfirmed transaction from the mempool

    let mempool_entry = rpc.get_mempool_entry(&txid)?;
    println!("Mempool entry: {:?}", mempool_entry);

    // Confirm the transaction by mining 1 block

    // Mine the confirming block to the same Miner mining address,
    // so the Miner keeps collecting block rewards too.
    let confirming_block_hashes = rpc.generate_to_address(1, &mining_address)?;
    let confirming_block_hash = confirming_block_hashes[0];

    // Extract all required transaction details
    // get_transaction (wallet-scoped) gives us decoded details + confirmations.
    let tx_info = miner_rpc.get_transaction(&txid, None)?;
    let block_height = rpc.get_block_info(&confirming_block_hash)?.height;

    let raw_tx = tx_info.transaction()?;
    let decoded: serde_json::Value = rpc.call(
        "decoderawtransaction",
        &[json!(tx_info.hex.to_lower_hex_string())],
    )?;

    let vin0 = &decoded["vin"][0];
    let prev_txid = vin0["txid"].as_str().unwrap();
    let prev_vout: u64 = vin0["vout"].as_u64().unwrap();

    let prev_raw: serde_json::Value =
        rpc.call("getrawtransaction", &[json!(prev_txid), json!(true)])?;
    let prev_out = &prev_raw["vout"][prev_vout as usize];
    let miner_input_address = prev_out["scriptPubKey"]["address"]
        .as_str()
        .unwrap()
        .to_string();
    let miner_input_amount = prev_out["value"].as_f64().unwrap();

    let mut trader_output_address = String::new();
    let mut trader_output_amount = 0.0;
    let mut miner_change_address = String::new();
    let mut miner_change_amount = 0.0;

    for vout in decoded["vout"].as_array().unwrap() {
        let addr = vout["scriptPubKey"]["address"].as_str().unwrap_or("");
        let value = vout["value"].as_f64().unwrap();
        if addr == trader_address.to_string() {
            trader_output_address = addr.to_string();
            trader_output_amount = value;
        } else {
            // The other output is Miner's change
            miner_change_address = addr.to_string();
            miner_change_amount = value;
        }
    }

    let fee = tx_info.fee.unwrap_or(SignedAmount::ZERO).to_btc();

    let mut out_file = File::create("../out.txt")?;
    writeln!(out_file, "{}", txid)?;
    writeln!(out_file, "{}", miner_input_address)?;
    writeln!(out_file, "{}", miner_input_amount)?;
    writeln!(out_file, "{}", trader_output_address)?;
    writeln!(out_file, "{}", trader_output_amount)?;
    writeln!(out_file, "{}", miner_change_address)?;
    writeln!(out_file, "{}", miner_change_amount)?;
    writeln!(out_file, "{}", fee)?;
    writeln!(out_file, "{}", block_height)?;
    writeln!(out_file, "{}", confirming_block_hash)?;

    println!("Wrote transaction details to out.txt");

    Ok(())
}
