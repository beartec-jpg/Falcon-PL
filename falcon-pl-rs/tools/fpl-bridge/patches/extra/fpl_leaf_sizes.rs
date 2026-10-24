fn main() {
    let vk = bridge::proof::get_proof().vk;
    let s = bitvm::chunk::api::api_generate_partial_script(&vk);
    let mut v: Vec<(usize, usize)> = s.iter().map(|x| x.len()).enumerate().collect();
    v.sort_by_key(|x| std::cmp::Reverse(x.1));
    let over = v.iter().filter(|x| x.1 > 390_000).count();
    println!("leaves {} over390k {} top: {:?}", v.len(), over, &v[..12]);
}
